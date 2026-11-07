/// Safety-regression tests of the 2026-11-07 crash-analysis fixes inside
/// `packages/ak_ffi_dart` (reports/codex-crash-analysis-2026-11-07.md):
///
/// * **C03** — [processAllocator] is ONE matched allocate/free pair
///   (package:ffi `malloc`), never two independent process-wide symbol
///   lookups; provenance is recorded via [processAllocatorSource].
/// * **C05** — the last-error/output readers are bounded by the caller
///   capacity: a returned length outside `0 <= len < cap`, a missing NUL
///   at the claimed position, or a missing NUL inside the buffer throws
///   [AkFfiProtocolException] instead of scanning past the allocation.
/// * **C06** — [akBufferCall] (the shared `buf`/`cap` runner behind the
///   pairing/task/sched `_bufCall`s) validates `cap == buffer.length`,
///   rejects unterminated success payloads, writes an explicit NUL into
///   the Dart copy, and zeroes the buffer on `-3` so a reused buffer sees
///   no stale residue.
/// * **C14** — multi-allocation call sites use one arena
///   (`using(..., processAllocator)`), so a later allocation failure frees
///   every earlier allocation (no leak).
///
/// No DLL involved: fakes drive the closure seams; only the real allocator
/// pair is exercised.
library;

import 'dart:convert' show utf8;
import 'dart:ffi';
import 'dart:typed_data' show Uint8List;

import 'package:ffi/ffi.dart' show malloc, using;
import 'package:test/test.dart';

import 'package:ak_ffi_dart/src/bindings.dart'
    show
        AkEngine,
        AkFfiProtocolException,
        AkStatus,
        processAllocator,
        processAllocatorSource;
import 'package:ak_ffi_dart/src/ffi_json.dart' show fromNativeUtf8;
import 'package:ak_ffi_dart/src/pairing_ffi.dart'
    show akBufferCall, readEngineLastError;

Matcher _throwsProtocol([String? needle]) => throwsA(
    isA<AkFfiProtocolException>().having(
        (e) => e.message, 'message', needle == null ? isNotNull : contains(needle)));

void main() {
  group('C03 配套分配器（单一成对 malloc/free）', () {
    test('processAllocator 就是 package:ffi 的 malloc（同一实现对象）', () {
      expect(identical(processAllocator, malloc), isTrue,
          reason: 'allocate 与 free 必须来自同一实现，不允许两个独立的'
              ' DynamicLibrary.process() 符号查找');
    });

    test('符号来源被记录且声明为成对实现', () {
      // Provenance record (C03): the description must name one package and
      // one matched pair — auditable without a debugger.
      expect(processAllocatorSource, contains('package:ffi'));
      expect(processAllocatorSource, contains('malloc'));
      expect(processAllocatorSource.toLowerCase(), contains('matched'));
    });

    test('分配→写入→释放循环保持数据完整（1000 轮，变长）', () {
      const rounds = 1000;
      for (var i = 0; i < rounds; i++) {
        final size = 1 + (i * 7) % 513;
        final bytes = Uint8List(size);
        for (var j = 0; j < size; j++) {
          bytes[j] = (i + j * 31) & 0xFF;
        }
        final pointer = processAllocator.allocate<Uint8>(size);
        try {
          pointer.asTypedList(size).setAll(0, bytes);
          expect(pointer.asTypedList(size), bytes,
              reason: 'round $i: written bytes must read back identical');
        } finally {
          processAllocator.free(pointer);
        }
      }
      // A canary allocation after the loop proves the pair is still healthy
      // (a cross-heap free anywhere above would have corrupted the heap).
      final canary = processAllocator.allocate<Uint8>(64);
      canary.asTypedList(64).fillRange(0, 64, 7);
      expect(canary.asTypedList(64).every((b) => b == 7), isTrue);
      processAllocator.free(canary);
    });

    test('并发持有的多块缓冲各自独立（交错分配/释放）', () {
      final a = processAllocator.allocate<Uint8>(128);
      final b = processAllocator.allocate<Uint8>(32);
      try {
        a.asTypedList(128).fillRange(0, 128, 1);
        b.asTypedList(32).fillRange(0, 32, 2);
        expect(a.asTypedList(128).every((byte) => byte == 1), isTrue);
        expect(b.asTypedList(32).every((byte) => byte == 2), isTrue);
      } finally {
        // Free in reverse ownership order (b before a): both must hit the
        // matching free of the SAME allocator.
        processAllocator.free(b);
        processAllocator.free(a);
      }
    });
  });

  group('C05 last-error 有界读取', () {
    // A fake `ak_engine_last_error` binding closure: writes [text] plus a
    // NUL when [terminate], reports [report] (defaults to text.length)
    // against whatever capacity the reader hands over (it starts at 256).
    int Function(Pointer<AkEngine>, Pointer<Uint8>, int) fakeLastError(
        {String text = '',
        int? report,
        bool terminate = true,
        bool fillAll = false}) {
      return (engine, buf, cap) {
        if (fillAll) {
          buf.asTypedList(cap).fillRange(0, cap, 0x41); // no NUL anywhere
          return report ?? cap;
        }
        final bytes = utf8.encode(text);
        buf.asTypedList(cap).setAll(0, bytes);
        if (terminate) {
          buf[bytes.length] = 0;
        }
        return report ?? bytes.length;
      };
    }

    final engine = nullptr.cast<AkEngine>();

    test('成功路径：返回长度 + NUL 一致时读出文本', () {
      expect(readEngineLastError(fakeLastError(text: 'boom 错误'), engine),
          'boom 错误');
      // Empty message decodes to '' (no detail).
      expect(readEngineLastError(fakeLastError(), engine), '');
    });

    test('返回长度 == cap（无界找 NUL 的旧缺陷形态）抛协议错误', () {
      // The buffer holds NOT ONE NUL byte; the reported length equals cap.
      // The old unbounded scan ran past the allocation; the bounded reader
      // must refuse.
      expect(() => readEngineLastError(fakeLastError(fillAll: true), engine),
          _throwsProtocol('outside'));
    });

    test('返回长度指向非 NUL 字节抛协议错误', () {
      expect(
          () => readEngineLastError(
              fakeLastError(text: 'abcdef', report: 3),
              engine),
          _throwsProtocol('not the promised NUL'));
    });

    test('返回长度超出 cap 抛协议错误', () {
      expect(
          () => readEngineLastError(
              fakeLastError(text: 'abc', report: 512),
              engine),
          _throwsProtocol('outside'));
    });

    test('-3 无损增长循环保持不变（先 -3 再成功）', () {
      var calls = 0;
      String read() => readEngineLastError((engine, buf, cap) {
            calls++;
            if (calls == 1) {
              return AkStatus.errBuffer; // stored message untouched
            }
            const text = '需要更大的缓冲区';
            final bytes = utf8.encode(text);
            buf.asTypedList(cap).setAll(0, bytes);
            buf[bytes.length] = 0;
            return bytes.length;
          }, engine);
      expect(read(), '需要更大的缓冲区');
      expect(calls, 2);
    });

    test('fromNativeUtf8 扫描形态：cap 内有 NUL 解码，无 NUL 抛协议错误', () {
      const cap = 16;
      final pointer = processAllocator.allocate<Uint8>(cap);
      try {
        pointer.asTypedList(cap).fillRange(0, cap, 0x7A); // 'z'
        pointer[5] = 0;
        expect(fromNativeUtf8(pointer, cap), 'zzzzz');
        // No NUL anywhere inside cap: bounded scan refuses.
        pointer[5] = 0x7A;
        expect(() => fromNativeUtf8(pointer, cap),
            _throwsProtocol('no NUL'));
      } finally {
        processAllocator.free(pointer);
      }
    });
  });

  group('C06 akBufferCall 契约（pairing/task/sched 共用 _bufCall）', () {
    int writePayload(Pointer<Uint8> buf, int cap, String text,
        {bool terminate = true, int code = 1}) {
      final bytes = utf8.encode(text);
      buf.asTypedList(cap).setAll(0, bytes);
      if (terminate) {
        buf[bytes.length] = 0;
      }
      return code;
    }

    test('成功：负载 + 显式 NUL 写入 Dart 缓冲', () {
      final buffer = Uint8List(64)..fillRange(0, 64, 0xAA);
      final code = akBufferCall(
          (buf, cap) => writePayload(buf, cap, '{"ok":true}'), buffer, 64);
      expect(code, 1);
      final payload = utf8.encode('{"ok":true}');
      expect(buffer.sublist(0, payload.length), payload);
      expect(buffer[payload.length], 0, reason: 'copy is self-terminating');
    });

    test('成功但 cap 内无 NUL：抛协议错误而不是吞下整段 cap', () {
      final buffer = Uint8List(8)..fillRange(0, 8, 0x55);
      expect(
          () => akBufferCall(
              // Exactly cap non-NUL bytes and no terminator anywhere: the
              // old code silently accepted all 8 bytes as the payload.
              (buf, cap) =>
                  writePayload(buf, cap, '12345678', terminate: false),
              buffer,
              8),
          _throwsProtocol('NUL'));
      expect(buffer.every((b) => b == 0x55), isTrue,
          reason: 'the Dart buffer stays untouched on the violation');
    });

    test('-3：清空缓冲尾部，复用无残留', () {
      final buffer = Uint8List(16)..fillRange(0, 16, 0xEE);
      final code = akBufferCall(
          (buf, cap) => AkStatus.errBuffer, buffer, 16);
      expect(code, AkStatus.errBuffer);
      expect(buffer.every((b) => b == 0), isTrue,
          reason: 'nothing consumed: a reused buffer must see zeros only');
    });

    test('0（队列为空）与其他负值：返回原码，缓冲不动', () {
      final buffer = Uint8List(8)..fillRange(0, 8, 0x11);
      expect(akBufferCall((buf, cap) => 0, buffer, 8), 0);
      expect(buffer.every((b) => b == 0x11), isTrue);
      expect(akBufferCall((buf, cap) => AkStatus.errNotFound, buffer, 8),
          AkStatus.errNotFound);
      expect(buffer.every((b) => b == 0x11), isTrue);
    });

    test('cap 与 Dart 缓冲长度不一致：立即 ArgumentError', () {
      final buffer = Uint8List(16);
      expect(() => akBufferCall((buf, cap) => 0, buffer, 15),
          throwsArgumentError,
          reason: 'the mismatch must surface at the seam, not as a '
              'RangeError mid-copy');
      expect(() => akBufferCall((buf, cap) => 0, buffer, 17),
          throwsArgumentError);
    });

    test('cap 0（空缓冲）可用且安全', () {
      expect(akBufferCall((buf, cap) => 0, Uint8List(0), 0), 0);
    });
  });

  group('C14 多分配入口无泄漏（arena 模式）', () {
    test('后续分配失败时，arena 释放之前所有分配', () {
      final flaky = _CountingAllocator(failOnNth: 3);
      expect(
          () => _simulateStartEntry(flaky),
          throwsStateError,
          reason: 'the third allocation fails exactly like an OOM at the '
              'old leak-prone call sites');
      expect(flaky.liveAllocations, 0,
          reason: 'every successful allocation was freed');
      expect(flaky.frees, 2,
          reason: 'allocations #1 and #2 were released by the arena');
    });

    test('正常路径：arena 释放全部（进出平衡）', () {
      final counting = _CountingAllocator();
      final code = _simulateStartEntry(counting);
      expect(code, 42);
      expect(counting.liveAllocations, 0);
      expect(counting.frees, 3);
    });
  });
}

/// The allocation shape of the fixed multi-allocation entries (task/
/// pairing/sched `start`-like calls): N sequential allocations inside one
/// `using` arena, one native "call", one status back.
int _simulateStartEntry(Allocator allocator) {
  return using((arena) {
    final spec = toNativeUtf8Local('{"spec_version":1}', allocator: arena);
    final id = arena<Uint8>(16);
    final out = arena<Uint8>(16);
    id[0] = 1;
    out[0] = spec[0];
    return 42;
  }, allocator);
}

/// [toNativeUtf8] mirror local to this test (the production one lives in
/// ffi_json.dart; a private copy keeps this file's imports minimal).
Pointer<Uint8> toNativeUtf8Local(String text, {Allocator? allocator}) {
  final heap = allocator ?? processAllocator;
  final bytes = utf8.encode(text);
  final pointer = heap.allocate<Uint8>(bytes.length + 1);
  for (var i = 0; i < bytes.length; i++) {
    pointer[i] = bytes[i];
  }
  pointer[bytes.length] = 0;
  return pointer;
}

/// Allocator wrapper that counts live blocks and can fail the Nth
/// allocation — the failure mode C14 is about.
final class _CountingAllocator implements Allocator {
  _CountingAllocator({this.failOnNth});

  final int? failOnNth;
  final List<Pointer<NativeType>> _live = [];
  int allocations = 0;
  int frees = 0;

  int get liveAllocations => _live.length;

  @override
  Pointer<T> allocate<T extends NativeType>(int byteCount, {int? alignment}) {
    allocations++;
    if (failOnNth == allocations) {
      throw StateError('native allocation of $byteCount bytes failed');
    }
    final pointer =
        processAllocator.allocate<T>(byteCount, alignment: alignment);
    _live.add(pointer);
    return pointer;
  }

  @override
  void free(Pointer pointer) {
    frees++;
    _live.remove(pointer);
    processAllocator.free(pointer);
  }
}
