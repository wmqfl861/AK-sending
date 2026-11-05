# Project-level configuration.
cmake_minimum_required(VERSION 3.13)
project(runner LANGUAGES CXX)

# The name of the executable created for the application. Change this to change
# the on-disk name of your application.
set(BINARY_NAME "ak_sending_app")
# The unique GTK application identifier for this application. See:
# https://wiki.gnome.org/HowDoI/ChooseApplicationID
set(APPLICATION_ID "dev.ak.ak_sending_app")

# Explicitly opt in to modern CMake behaviors to avoid warnings with recent
# versions of CMake.
cmake_policy(SET CMP0063 NEW)

# Load bundled libraries from the lib/ directory relative to the binary.
set(CMAKE_INSTALL_RPATH "$ORIGIN/lib")

# Root filesystem for cross-building.
if(FLUTTER_TARGET_PLATFORM_SYSROOT)
  set(CMAKE_SYSROOT ${FLUTTER_TARGET_PLATFORM_SYSROOT})
  set(CMAKE_FIND_ROOT_PATH ${CMAKE_SYSROOT})
  set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
  set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
  set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
  set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
endif()

# Define build configuration options.
if(NOT CMAKE_BUILD_TYPE AND NOT CMAKE_CONFIGURATION_TYPES)
  set(CMAKE_BUILD_TYPE "Debug" CACHE
    STRING "Flutter build mode" FORCE)
  set_property(CACHE CMAKE_BUILD_TYPE PROPERTY STRINGS
    "Debug" "Profile" "Release")
endif()

# Compilation settings that should be applied to most targets.
#
# Be cautious about adding new options here, as plugins use this function by
# default. In most cases, you should add new options to specific targets instead
# of modifying this function.
function(APPLY_STANDARD_SETTINGS TARGET)
  target_compile_features(${TARGET} PUBLIC cxx_std_14)
  target_compile_options(${TARGET} PRIVATE -Wall -Werror)
  target_compile_options(${TARGET} PRIVATE "$<$<NOT:$<CONFIG:Debug>>:-O3>")
  target_compile_definitions(${TARGET} PRIVATE "$<$<NOT:$<CONFIG:Debug>>:NDEBUG>")
endfunction()

# Flutter library and tool build rules.
set(FLUTTER_MANAGED_DIR "${CMAKE_CURRENT_SOURCE_DIR}/flutter")
add_subdirectory(${FLUTTER_MANAGED_DIR})

# System-level dependencies.
find_package(PkgConfig REQUIRED)
pkg_check_modules(GTK REQUIRED IMPORTED_TARGET gtk+-3.0)

# Application build; see runner/CMakeLists.txt.
add_subdirectory("runner")

# Run the Flutter tool portions of the build. This must not be removed.
add_dependencies(${BINARY_NAME} flutter_assemble)

# Only the install-generated bundle's copy of the executable will launch
# correctly, since the resources must in the right relative locations. To avoid
# people trying to run the unbundled copy, put it in a subdirectory instead of
# the default top-level location.
set_target_properties(${BINARY_NAME}
  PROPERTIES
  RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/intermediates_do_not_run"
)


# Generated plugin build rules, which manage building the plugins and adding
# them to the application.
include(flutter/generated_plugins.cmake)


# === Installation ===
# By default, "installing" just makes a relocatable bundle in the build
# directory.
set(BUILD_BUNDLE_DIR "${PROJECT_BINARY_DIR}/bundle")
if(CMAKE_INSTALL_PREFIX_INITIALIZED_TO_DEFAULT)
  set(CMAKE_INSTALL_PREFIX "${BUILD_BUNDLE_DIR}" CACHE PATH "..." FORCE)
endif()

# Start with a clean build bundle directory every time.
install(CODE "
  file(REMOVE_RECURSE \"${BUILD_BUNDLE_DIR}/\")
  " COMPONENT Runtime)

set(INSTALL_BUNDLE_DATA_DIR "${CMAKE_INSTALL_PREFIX}/data")
set(INSTALL_BUNDLE_LIB_DIR "${CMAKE_INSTALL_PREFIX}/lib")

install(TARGETS ${BINARY_NAME} RUNTIME DESTINATION "${CMAKE_INSTALL_PREFIX}"
  COMPONENT Runtime)

install(FILES "${FLUTTER_ICU_DATA_FILE}" DESTINATION "${INSTALL_BUNDLE_DATA_DIR}"
  COMPONENT Runtime)

install(FILES "${FLUTTER_LIBRARY}" DESTINATION "${INSTALL_BUNDLE_LIB_DIR}"
  COMPONENT Runtime)

foreach(bundled_library ${PLUGIN_BUNDLED_LIBRARIES})
  install(FILES "${bundled_library}"
    DESTINATION "${INSTALL_BUNDLE_LIB_DIR}"
    COMPONENT Runtime)
endforeach(bundled_library)

# --- ak-ffi: bundle libak_ffi.so next to the bundle exe (Linux twin of the
# windows/runner/CMakeLists.txt DLL copy). The Dart probe (dll_path.dart tier 2)
# opens "<dir of Platform.resolvedExecutable>/libak_ffi.so", and the runnable
# Linux artifact is this install bundle, so the .so is installed at the bundle
# root beside the executable (not into lib/; the Dart side passes a full path,
# so the $ORIGIN/lib rpath is not consulted for it).
# Resolution order (first hit wins; see flutter_app/README.md "DLL 三级探测"):
#   1. AK_FFI_SO cache variable — cmake -DAK_FFI_SO=/path/libak_ffi.so ...
#   2. AK_FFI_SO / AK_FFI_DLL environment variables (captured at configure
#      time; AK_FFI_DLL matches the documented tier-1 override)
#   3. <repo>/target/release/libak_ffi.so from a native
#      `cargo build -p ak-ffi --release`
#   4. <repo>/target/x86_64-unknown-linux-gnu/release/libak_ffi.so from a
#      cross `cargo build -p ak-ffi --release --target x86_64-unknown-linux-gnu`
#      (this file sits at flutter_app/linux, so the repo root is two levels up)
set(AK_FFI_SO "" CACHE STRING "Path to libak_ffi.so to bundle next to the bundle exe; empty = auto-detect (env AK_FFI_SO/AK_FFI_DLL, then <repo>/target/{release,x86_64-unknown-linux-gnu/release}/libak_ffi.so)")

set(_ak_ffi_so_src "")
if(AK_FFI_SO)
  set(_ak_ffi_so_src "${AK_FFI_SO}")
elseif(DEFINED ENV{AK_FFI_SO} AND NOT "$ENV{AK_FFI_SO}" STREQUAL "")
  set(_ak_ffi_so_src "$ENV{AK_FFI_SO}")
elseif(DEFINED ENV{AK_FFI_DLL} AND NOT "$ENV{AK_FFI_DLL}" STREQUAL "")
  set(_ak_ffi_so_src "$ENV{AK_FFI_DLL}")
elseif(EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/../../target/release/libak_ffi.so")
  set(_ak_ffi_so_src "${CMAKE_CURRENT_SOURCE_DIR}/../../target/release/libak_ffi.so")
elseif(EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/../../target/x86_64-unknown-linux-gnu/release/libak_ffi.so")
  set(_ak_ffi_so_src "${CMAKE_CURRENT_SOURCE_DIR}/../../target/x86_64-unknown-linux-gnu/release/libak_ffi.so")
endif()

if(_ak_ffi_so_src)
  install(FILES "${_ak_ffi_so_src}"
    DESTINATION "${CMAKE_INSTALL_PREFIX}"
    COMPONENT Runtime)
  message(STATUS "ak-ffi: bundling libak_ffi.so from: ${_ak_ffi_so_src}")
else()
  message(WARNING "libak_ffi.so not found (no AK_FFI_SO override/env, none under <repo>/target/{release,x86_64-unknown-linux-gnu/release}). Build it on Linux with: cargo build -p ak-ffi --release — otherwise the bundled app starts without a transfer engine (see linux/BUILD.md).")
endif()

# Copy the native assets provided by the build.dart from all packages.
set(NATIVE_ASSETS_DIR "${PROJECT_BUILD_DIR}native_assets/linux/")
install(DIRECTORY "${NATIVE_ASSETS_DIR}"
   DESTINATION "${INSTALL_BUNDLE_LIB_DIR}"
   COMPONENT Runtime)

# Fully re-copy the assets directory on each build to avoid having stale files
# from a previous install.
set(FLUTTER_ASSET_DIR_NAME "flutter_assets")
install(CODE "
  file(REMOVE_RECURSE \"${INSTALL_BUNDLE_DATA_DIR}/${FLUTTER_ASSET_DIR_NAME}\")
  " COMPONENT Runtime)
install(DIRECTORY "${PROJECT_BUILD_DIR}/${FLUTTER_ASSET_DIR_NAME}"
  DESTINATION "${INSTALL_BUNDLE_DATA_DIR}" COMPONENT Runtime)

# Install the AOT library on non-Debug builds only.
if(NOT CMAKE_BUILD_TYPE MATCHES "Debug")
  install(FILES "${AOT_LIBRARY}" DESTINATION "${INSTALL_BUNDLE_LIB_DIR}"
    COMPONENT Runtime)
endif()
