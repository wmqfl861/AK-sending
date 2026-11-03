cmake_minimum_required(VERSION 3.14)
project(runner LANGUAGES CXX)

# Define the application target. To change its name, change BINARY_NAME in the
# top-level CMakeLists.txt, not the value here, or `flutter run` will no longer
# work.
#
# Any new source files that you add to the application should be added here.
add_executable(${BINARY_NAME} WIN32
  "flutter_window.cpp"
  "main.cpp"
  "utils.cpp"
  "win32_window.cpp"
  "${FLUTTER_MANAGED_DIR}/generated_plugin_registrant.cc"
  "Runner.rc"
  "runner.exe.manifest"
)

# Apply the standard set of build settings. This can be removed for applications
# that need different build settings.
apply_standard_settings(${BINARY_NAME})

# Add preprocessor definitions for the build version.
target_compile_definitions(${BINARY_NAME} PRIVATE "FLUTTER_VERSION=\"${FLUTTER_VERSION}\"")
target_compile_definitions(${BINARY_NAME} PRIVATE "FLUTTER_VERSION_MAJOR=${FLUTTER_VERSION_MAJOR}")
target_compile_definitions(${BINARY_NAME} PRIVATE "FLUTTER_VERSION_MINOR=${FLUTTER_VERSION_MINOR}")
target_compile_definitions(${BINARY_NAME} PRIVATE "FLUTTER_VERSION_PATCH=${FLUTTER_VERSION_PATCH}")
target_compile_definitions(${BINARY_NAME} PRIVATE "FLUTTER_VERSION_BUILD=${FLUTTER_VERSION_BUILD}")

# Disable Windows macros that collide with C++ standard library functions.
target_compile_definitions(${BINARY_NAME} PRIVATE "NOMINMAX")

# Add dependency libraries and include directories. Add any application-specific
# dependencies here.
target_link_libraries(${BINARY_NAME} PRIVATE flutter flutter_wrapper_app)
target_link_libraries(${BINARY_NAME} PRIVATE "dwmapi.lib")
target_include_directories(${BINARY_NAME} PRIVATE "${CMAKE_SOURCE_DIR}")

# Run the Flutter tool portions of the build. This must not be removed.
add_dependencies(${BINARY_NAME} flutter_assemble)

# --- ak-ffi: bundle ak_ffi.dll next to the exe (append-only wave-11 step) ---
# Resolution order (first hit wins; see flutter_app/README.md "DLL 三级探测"):
#   1. AK_FFI_DLL cache variable  — cmake -DAK_FFI_DLL=C:/path/ak_ffi.dll ...
#   2. AK_FFI_DLL environment variable (captured at configure time)
#   3. <repo>/target/release/ak_ffi.dll from `cargo build -p ak-ffi --release`
#      (this file sits at flutter_app/windows/runner, so the repo root is
#      three levels up)
set(AK_FFI_DLL "" CACHE STRING "Path to ak_ffi.dll to bundle next to the exe; empty = auto-detect (env AK_FFI_DLL, then <repo>/target/release/ak_ffi.dll)")

set(_ak_ffi_dll_src "")
if(AK_FFI_DLL)
  set(_ak_ffi_dll_src "${AK_FFI_DLL}")
elseif(DEFINED ENV{AK_FFI_DLL} AND NOT "$ENV{AK_FFI_DLL}" STREQUAL "")
  set(_ak_ffi_dll_src "$ENV{AK_FFI_DLL}")
elseif(EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/../../../target/release/ak_ffi.dll")
  set(_ak_ffi_dll_src "${CMAKE_CURRENT_SOURCE_DIR}/../../../target/release/ak_ffi.dll")
endif()

if(_ak_ffi_dll_src)
  add_custom_command(TARGET ${BINARY_NAME} POST_BUILD
    COMMAND ${CMAKE_COMMAND} -E copy_if_different
            "${_ak_ffi_dll_src}"
            "$<TARGET_FILE_DIR:${BINARY_NAME}>/ak_ffi.dll"
    COMMENT "Bundling ak_ffi.dll from: ${_ak_ffi_dll_src}"
    VERBATIM)
else()
  message(WARNING "ak_ffi.dll not found (no AK_FFI_DLL override, no ${CMAKE_CURRENT_SOURCE_DIR}/../../../target/release/ak_ffi.dll). Build it with: cargo build -p ak-ffi --release — otherwise the app starts without a transfer engine.")
endif()
