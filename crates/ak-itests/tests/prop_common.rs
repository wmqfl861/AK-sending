//! 随机性质套件（tests/property_random.rs）的共享生成器：确定性随机树、
//! 传输旋钮抽取、种子上下文断言助手。
//!
//! 双重身份说明：本文件既被 `tests/property_random.rs` 以 `mod prop_common;`
//! 引用，又被 cargo 自动当作一个独立的（空）集成测试目标编译，因此顶部
//! `allow(dead_code)` 与 `tests/common/mod.rs` 的手法一致。
//!
//! 层级声明：与 crate 其余测试一致的 L2 环回形态——随机树经完整引擎
//! （QUIC + TLS + 检查点）在 127.0.0.1 上传输，不是真机/跨网主张。
//!
//! 确定性：一切随机都来自 `common::Lcg`（Knuth 常数，无随机依赖），
//! 种子号完全决定树形、文件字节与旋钮取值；任何断言失败消息都带
//! `seed=<n>` 与生成器摘要（文件数/目录数/总字节/树哈希/旋钮），
//! 用同一种子重跑即可复现。
//!
//! 生成分布（`TreeParams` 控制上限）：
//! - 文件数 0..=max_files（夹到至少 1——本套件只主张“有内容的传输”，
//!   全空树的传输形态不在此覆盖，见 property_random 模块文档的已知限制）；
//! - 单文件大小 0..=512 KiB（含 0 字节空文件）；
//! - 目录深度 0..=max_depth；
//! - 名称混合：纯 ASCII、CJK、内部空格、多点扩展（a.b.c）、单字符
//!   （ASCII/CJK/数字）、点前缀隐藏名、以及 ASCII+CJK+空格+内点的混合；
//! - 空目录随机：0..=max_empty_dirs 条新链（叶子必为新目录）。
//!
//! 名称合法性由构造保证：不以点/空格结尾、无点组件、无控制字符、
//! 非 Windows 保留设备名（CON/PRN/AUX/NUL/COM1-9/LPT1-9 的镜像列表，
//! ak-core 里该列表私有）；文件名尾部带每树唯一的两位序号，加上
//! 全树已用路径集合的确定性消歧（冲突时追加 'q'），保证任何两个
//! 条目都不会落在同一路径上。
#![allow(dead_code)]
#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::path::Path;

// `#[path]` 显式定位：本文件既作为 property_random 的嵌套模块编译（非根
// 模块文件的子模块默认会去找 tests/prop_common/common.rs），也作为独立
// 测试目标编译（crate 根），两种身份下都指向 tests/common/mod.rs。
#[path = "common/mod.rs"]
mod common;

pub use common::{
    snapshot_tree, try_run_transfer_opts, write_file, Lcg, TransferOpts, Tree, PART_SUFFIX,
};

/// 种子加盐：种子号都很小（1..=25），与固定盐异或后作 LCG 状态，避免
/// 前几步输出结构性偏弱。种子号本身保持人类可读（失败消息里报 seed=7）。
pub const TREE_SEED_SALT: u64 = 0xAB1E_5EED_2026_1104;

/// 中断点抽取的独立盐：与树生成流互不干扰（中断点依赖树的总 chunk 数，
/// 树规划完成后才确定，故用第二个种子流）。
pub const ABORT_SEED_SALT: u64 = 0xC0DE_C0FF_EE00_1104;

/// 名义大小上限：0..=512 KiB（任务规格）。
pub const MAX_FILE_BYTES: u64 = 512 * 1024;

/// 名称形状池：CJK 字符。
const CJK: [char; 10] = ['文', '档', '数', '据', '传', '输', '节', '点', '备', '份'];

/// 名称形状池：ASCII 字母数字（小写 + 数字，36 个）。
const ALNUM: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// Windows 保留设备名（ak-core manifest::RESERVED_NAMES 的镜像；那里是
/// 私有常量）。以“首个点前的词干、大写比较”判定。
const RESERVED_STEMS: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// FNV-1a 64 常量：失败消息里的“树哈希”用（纯诊断，不是安全哈希）。
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv_mix(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn stem_is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("");
    RESERVED_STEMS.contains(&stem.to_ascii_uppercase().as_str())
}

fn alnum_char(rng: &mut Lcg) -> char {
    ALNUM[rng.below(36) as usize] as char
}

fn cjk_char(rng: &mut Lcg) -> char {
    CJK[rng.below(CJK.len() as u64) as usize]
}

/// 抽一个名称形状。每个 kind 的 LCG 消耗次数固定（与参数无关），保证同一种
/// 子在同参数下逐次复现。所有形状都以字母数字或 CJK 结尾（合法性由构造保证）。
fn draw_name(rng: &mut Lcg, kind: u64) -> String {
    let mut name = match kind {
        // 纯 ASCII 字母数字。
        0 => {
            let len = 3 + rng.below(6) as usize;
            (0..len).map(|_| alnum_char(rng)).collect()
        }
        // 纯 CJK。
        1 => {
            let len = 1 + rng.below(3) as usize;
            (0..len).map(|_| cjk_char(rng)).collect()
        }
        // 内部空格：两个词，中间一个空格。
        2 => {
            let head = 1 + rng.below(4) as usize;
            let tail = 1 + rng.below(4) as usize;
            let mut name: String = (0..head).map(|_| alnum_char(rng)).collect();
            name.push(' ');
            name.extend((0..tail).map(|_| alnum_char(rng)));
            name
        }
        // 多点扩展：stem.e1.e2。
        3 => {
            let stem = 2 + rng.below(4) as usize;
            let ext1 = 1 + rng.below(3) as usize;
            let ext2 = 1 + rng.below(3) as usize;
            let mut name: String = (0..stem).map(|_| alnum_char(rng)).collect();
            for len in [ext1, ext2] {
                name.push('.');
                name.extend((0..len).map(|_| alnum_char(rng)));
            }
            name
        }
        // 单字符：ASCII 字母数字 / CJK / 数字。
        4 => match rng.below(3) {
            0 => alnum_char(rng).to_string(),
            1 => cjk_char(rng).to_string(),
            _ => char::from(b'0' + rng.below(10) as u8).to_string(),
        },
        // 点前缀隐藏名：.conf 风格。
        5 => {
            let len = 1 + rng.below(4) as usize;
            let mut name = String::from(".");
            name.extend((0..len).map(|_| alnum_char(rng)));
            name
        }
        // 混合：ASCII 词 + 空格 + CJK + 内点 + ASCII 尾。
        _ => {
            let head = 1 + rng.below(3) as usize;
            let han = 1 + rng.below(2) as usize;
            let tail = 1 + rng.below(2) as usize;
            let mut name: String = (0..head).map(|_| alnum_char(rng)).collect();
            name.push(' ');
            name.extend((0..han).map(|_| cjk_char(rng)));
            name.push('.');
            name.extend((0..tail).map(|_| alnum_char(rng)));
            name
        }
    };
    if stem_is_reserved(&name) {
        // 命中保留设备名（如抽到 "con.x"）：确定性补一个字母使其合法。
        name.push('q');
    }
    name
}

/// 树形状参数：三档（25 种子传输 / 10 种子中断续传 / 快速回归）。
pub struct TreeParams {
    pub label: &'static str,
    /// 文件数抽取上限（0..=max_files，夹到至少 1）。
    pub max_files: u64,
    /// 目录深度上限（0..=max_depth）。
    pub max_depth: u64,
    /// 追加空目录链数上限（0..=max_empty_dirs）。
    pub max_empty_dirs: u64,
    /// 单文件字节上限（0..=max_file_bytes）。
    pub max_file_bytes: u64,
    /// Some(min)：规划后若总 chunk 数不足，确定性加长 0 号文件（可超名义
    /// 大小上限）——中断续传性质需要足够的 chunk 才有“随机中断点”可言。
    pub min_total_chunks: Option<u64>,
}

impl TreeParams {
    /// 25 种子随机传输套件：0-40 文件、深度 0-6、0-512 KiB。
    pub fn release_transfer() -> Self {
        Self {
            label: "release-transfer",
            max_files: 40,
            max_depth: 6,
            max_empty_dirs: 4,
            max_file_bytes: MAX_FILE_BYTES,
            min_total_chunks: None,
        }
    }

    /// 10 种子中断续传套件：规模减半（0-20 文件），其余同上。
    pub fn release_resume() -> Self {
        Self {
            label: "release-resume",
            max_files: 20,
            max_depth: 6,
            max_empty_dirs: 3,
            max_file_bytes: MAX_FILE_BYTES,
            min_total_chunks: Some(8),
        }
    }

    /// 快速回归（debug 也跑）：种子 1-3，文件 <=6、深度 <=3。
    pub fn fast_debug() -> Self {
        Self {
            label: "fast-debug",
            max_files: 6,
            max_depth: 3,
            max_empty_dirs: 2,
            max_file_bytes: MAX_FILE_BYTES,
            min_total_chunks: None,
        }
    }
}

/// 生成器摘要：进失败消息，保证“看到消息即可复现”。
#[derive(Debug, Clone)]
pub struct GenSummary {
    pub seed: u64,
    pub suite: &'static str,
    pub files: usize,
    pub dirs: usize,
    pub bytes: u64,
    pub empty_files: usize,
    /// 按 chunk_kb 折算的总 chunk 数（每文件 ceil(size/chunk)，空文件 0）。
    pub total_chunks: u64,
    pub chunk_kb: u32,
    pub multistream: bool,
    pub tree_hash: u64,
}

impl GenSummary {
    /// 一行式摘要：种子号 + 套件 + 文件/目录/字节/chunk + 旋钮 + 树哈希。
    pub fn summary(&self) -> String {
        format!(
            "seed={} suite={} files={} dirs={} bytes={} empty_files={} chunks={} \
             chunk_kb={} multistream={} tree_hash={:#018x}",
            self.seed,
            self.suite,
            self.files,
            self.dirs,
            self.bytes,
            self.empty_files,
            self.total_chunks,
            self.chunk_kb,
            self.multistream,
            self.tree_hash
        )
    }

    /// 断言消息前缀（带换行，后续跟断言细节）。
    pub fn ctx(&self) -> String {
        format!("{}\n", self.summary())
    }
}

fn join_rel(prefix: &str, component: &str) -> String {
    if prefix.is_empty() {
        component.to_string()
    } else {
        format!("{prefix}/{component}")
    }
}

/// 规划状态：已用路径集合（'/' 分隔全相对路径）保证文件/目录不冲突。
struct Planner {
    used_files: BTreeSet<String>,
    used_dirs: BTreeSet<String>,
    /// 已用作单字符文件名的字符（跨树唯一，防大小写不敏感文件系统上撞名）。
    used_single: BTreeSet<char>,
}

impl Planner {
    /// 放置一条目录链：已存在的目录直接复用；被文件占据的层确定性追加 'q'
    /// 消歧。返回链尾前缀（链为空时为 ""）。
    fn place_dir_chain(&mut self, components: &[String]) -> String {
        let mut prefix = String::new();
        for component in components {
            let mut component = component.clone();
            let mut rel = join_rel(&prefix, &component);
            while self.used_files.contains(&rel) {
                component.push('q');
                rel = join_rel(&prefix, &component);
            }
            self.used_dirs.insert(rel.clone());
            prefix = rel;
        }
        prefix
    }

    /// 放置文件叶子：与已有文件或目录同路径时追加 'q' 消歧。
    fn place_leaf(&mut self, prefix: &str, mut leaf: String) -> String {
        let mut rel = join_rel(prefix, &leaf);
        while self.used_files.contains(&rel) || self.used_dirs.contains(&rel) {
            leaf.push('q');
            rel = join_rel(prefix, &leaf);
        }
        self.used_files.insert(rel.clone());
        rel
    }

    /// 放置一条空目录链：叶子必须是新目录（真·空目录）。任何一层被文件
    /// 占据则消歧；叶子已存在（无论文件/目录）则放弃本条（确定性跳过）。
    fn place_empty_chain(&mut self, components: &[String]) -> bool {
        let mut prefix = String::new();
        for (index, component) in components.iter().enumerate() {
            let mut component = component.clone();
            let mut rel = join_rel(&prefix, &component);
            while self.used_files.contains(&rel) {
                component.push('q');
                rel = join_rel(&prefix, &component);
            }
            let is_leaf = index == components.len() - 1;
            if self.used_dirs.contains(&rel) {
                if is_leaf {
                    return false; // 叶子已存在：不是新空目录，跳过。
                }
            } else {
                self.used_dirs.insert(rel.clone());
            }
            prefix = rel;
        }
        true
    }
}

/// 文件叶子名：kind 4（单字符）用全局字符池保唯一（池尽则退化为带序号的
/// ASCII 名）；其余 kind 尾接每树唯一两位序号，天然互不冲突。
fn draw_leaf_name(
    rng: &mut Lcg,
    kind: u64,
    index: usize,
    used_single: &mut BTreeSet<char>,
) -> String {
    if kind == 4 {
        for _ in 0..16 {
            let name = draw_name(rng, 4);
            if let Some(single) = name.chars().next() {
                if !used_single.contains(&single) {
                    used_single.insert(single);
                    return name;
                }
            }
        }
        // 池耗尽（本套件规模不可能）：退化为带序号 ASCII 名。
        return format!("{}{index:02}", draw_name(rng, 0));
    }
    format!("{}{index:02}", draw_name(rng, kind))
}

/// 传输旋钮抽取：chunk_kb ∈ {16,64,256}，multistream ∈ {false,true}。
fn draw_knobs(rng: &mut Lcg) -> (u32, bool) {
    let chunk_kb = [16u32, 64, 256][rng.below(3) as usize];
    let multistream = rng.below(2) == 1;
    (chunk_kb, multistream)
}

/// 在 `root` 下按种子生成随机树并落盘；返回可复现摘要。
///
/// 落盘后立即与 `snapshot_tree` 对账（目录集合、文件集合、字节数）——
/// 生成器记账错误在传输前就以种子上下文暴露，不留给引擎测试背锅。
pub fn build_random_tree(root: &Path, seed: u64, params: &TreeParams) -> GenSummary {
    let mut rng = Lcg::new(seed ^ TREE_SEED_SALT);
    let (chunk_kb, multistream) = draw_knobs(&mut rng);
    let chunk_bytes = u64::from(chunk_kb) * 1024;

    // ---- 规划：文件（深度/链/叶子/大小） --------------------------------
    let file_count = (rng.below(params.max_files + 1) as usize).max(1);
    let mut planner = Planner {
        used_files: BTreeSet::new(),
        used_dirs: BTreeSet::new(),
        used_single: BTreeSet::new(),
    };
    let mut rels: Vec<String> = Vec::with_capacity(file_count);
    let mut sizes: Vec<u64> = Vec::with_capacity(file_count);
    for index in 0..file_count {
        let depth = rng.below(params.max_depth + 1) as usize;
        let chain: Vec<String> = (0..depth)
            .map(|_| {
                let kind = rng.below(6);
                draw_name(&mut rng, kind)
            })
            .collect();
        let kind = rng.below(7);
        let leaf = draw_leaf_name(&mut rng, kind, index, &mut planner.used_single);
        let size = rng.below(params.max_file_bytes + 1);
        let prefix = planner.place_dir_chain(&chain);
        rels.push(planner.place_leaf(&prefix, leaf));
        sizes.push(size);
    }

    // ---- 规划：空目录链 ---------------------------------------------------
    let empty_wanted = rng.below(params.max_empty_dirs + 1) as usize;
    for _ in 0..empty_wanted {
        let depth = 1 + rng.below(params.max_depth) as usize;
        let chain: Vec<String> = (0..depth)
            .map(|_| {
                let kind = rng.below(6);
                draw_name(&mut rng, kind)
            })
            .collect();
        // 叶子已存在时确定性跳过本条（不是新空目录）。
        planner.place_empty_chain(&chain);
    }

    // ---- 续传套件的 chunk 兜底：总 chunk 不足则确定性加长 0 号文件 -------
    let chunks_of = |size: u64| size.div_ceil(chunk_bytes);
    if let Some(min_chunks) = params.min_total_chunks {
        let total: u64 = sizes.iter().map(|size| chunks_of(*size)).sum();
        if total < min_chunks {
            sizes[0] += (min_chunks - total) * chunk_bytes;
        }
    }

    // ---- 落盘：目录按序创建，文件字节继续从同一种子流生成 -----------------
    for dir_rel in &planner.used_dirs {
        std::fs::create_dir_all(root.join(dir_rel)).unwrap();
    }
    let mut bytes = 0u64;
    let mut empty_files = 0usize;
    for (rel, size) in rels.iter().zip(&sizes) {
        let content = rng.bytes(*size as usize);
        if content.is_empty() {
            empty_files += 1;
        }
        bytes += content.len() as u64;
        write_file(&root.join(rel), &content);
    }

    // ---- 对账：盘上事实必须与规划一致 -------------------------------------
    let tree = snapshot_tree(root);
    assert_eq!(
        tree.dirs, planner.used_dirs,
        "generator bookkeeping: dirs on disk differ from plan (seed {seed}, {})",
        params.label
    );
    let planned: BTreeSet<&String> = rels.iter().collect();
    let on_disk: BTreeSet<&String> = tree.files.keys().collect();
    assert_eq!(
        planned, on_disk,
        "generator bookkeeping: files on disk differ from plan (seed {seed}, {})",
        params.label
    );
    for (rel, size) in rels.iter().zip(&sizes) {
        assert_eq!(
            tree.files[rel].len() as u64,
            *size,
            "generator bookkeeping: size mismatch for {rel} (seed {seed})"
        );
    }
    let total_chunks: u64 = sizes.iter().map(|size| chunks_of(*size)).sum();

    // ---- 树哈希（诊断用）：目录名有序 + 文件（路径/长度/字节）有序 -------
    let mut hash = FNV_OFFSET;
    for dir in &tree.dirs {
        hash = fnv_mix(hash, dir.as_bytes());
        hash = fnv_mix(hash, b"/");
    }
    for (rel, content) in &tree.files {
        hash = fnv_mix(hash, rel.as_bytes());
        hash = fnv_mix(hash, &content.len().to_le_bytes());
        hash = fnv_mix(hash, content);
    }

    GenSummary {
        seed,
        suite: params.label,
        files: rels.len(),
        dirs: tree.dirs.len(),
        bytes,
        empty_files,
        total_chunks,
        chunk_kb,
        multistream,
        tree_hash: hash,
    }
}

/// 种子上下文版树比对：与 common::assert_trees_identical 同规则（目录集合、
/// 文件集合、大小、逐字节），但每条失败消息都带 `ctx`（种子号 + 摘要）。
pub fn assert_trees_identical_ctx(ctx: &str, src_root: &Path, dst_root: &Path) {
    let src = snapshot_tree(src_root);
    let dst = snapshot_tree(dst_root);

    let missing_dirs: Vec<&String> = src.dirs.difference(&dst.dirs).collect();
    let extra_dirs: Vec<&String> = dst.dirs.difference(&src.dirs).collect();
    assert!(
        missing_dirs.is_empty() && extra_dirs.is_empty(),
        "{ctx}directory structure differs\nmissing: {missing_dirs:?}\nextra: {extra_dirs:?}"
    );

    let missing: Vec<&String> = src
        .files
        .keys()
        .filter(|name| !dst.files.contains_key(*name))
        .collect();
    let extra: Vec<&String> = dst
        .files
        .keys()
        .filter(|name| !src.files.contains_key(*name))
        .collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{ctx}file sets differ\nmissing: {missing:?}\nextra: {extra:?}"
    );

    let wrong_size: Vec<(&String, usize, usize)> = src
        .files
        .iter()
        .filter_map(|(name, bytes)| {
            let got = &dst.files[name];
            (got.len() != bytes.len()).then_some((name, bytes.len(), got.len()))
        })
        .collect();
    assert!(
        wrong_size.is_empty(),
        "{ctx}file sizes differ (name, expected, got): {wrong_size:?}"
    );

    let mut wrong_bytes: Vec<&String> = Vec::new();
    for (name, bytes) in &src.files {
        if &dst.files[name] != bytes {
            wrong_bytes.push(name);
            if wrong_bytes.len() >= 10 {
                break;
            }
        }
    }
    assert!(
        wrong_bytes.is_empty(),
        "{ctx}file contents differ for: {wrong_bytes:?}"
    );
}

/// 种子上下文版 .akpart 残留检查。
pub fn assert_no_part_files_ctx(ctx: &str, root: &Path) {
    let tree: Tree = snapshot_tree(root);
    let parts: Vec<&String> = tree
        .files
        .keys()
        .filter(|name| name.ends_with(PART_SUFFIX))
        .collect();
    assert!(parts.is_empty(), "{ctx}leftover part files: {parts:?}");
}
