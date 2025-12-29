//! Build script for guacr-rdp
//!
//! Generates Rust FFI bindings to FreeRDP using bindgen.
//! Requires freerdp2-dev (or equivalent) to be installed.
//!
//! Supports multiple discovery methods:
//! - pkg-config (Linux, macOS)
//! - vcpkg (Windows)
//! - Environment variables (FREERDP_INCLUDE_DIR, FREERDP_LIB_DIR)
//!
//! If FreeRDP is not found, a stub binding is generated that will
//! produce runtime errors if used. This allows the crate to compile
//! without FreeRDP for documentation and development purposes.

use std::env;
use std::fs;
use std::path::PathBuf;

/// Try to find FreeRDP using multiple methods
/// Returns include paths if found, None otherwise
fn find_freerdp() -> Option<Vec<PathBuf>> {
    // Method 1: Environment variables (highest priority, for CI/vcpkg)
    if let Ok(include_dir) = env::var("FREERDP_INCLUDE_DIR") {
        let include_path = PathBuf::from(&include_dir);
        if include_path.exists() {
            eprintln!(
                "cargo:warning=Found FreeRDP via FREERDP_INCLUDE_DIR: {}",
                include_dir
            );

            // Detect FreeRDP version by checking for freerdp3 subdirectory
            let freerdp3_dir = include_path.join("freerdp3");
            let is_v3 = freerdp3_dir.exists();

            // Also set up library linking if FREERDP_LIB_DIR is set
            if let Ok(lib_dir) = env::var("FREERDP_LIB_DIR") {
                println!("cargo:rustc-link-search=native={}", lib_dir);
                if is_v3 {
                    // FreeRDP 3.x library names
                    println!("cargo:rustc-link-lib=freerdp3");
                    println!("cargo:rustc-link-lib=winpr3");
                    println!("cargo:rustc-link-lib=freerdp-client3");
                } else {
                    // FreeRDP 2.x library names
                    println!("cargo:rustc-link-lib=freerdp2");
                    println!("cargo:rustc-link-lib=winpr2");
                }
            }

            // Include paths for FreeRDP 3.x (vcpkg layout)
            // Headers are in: include/freerdp3/freerdp/freerdp.h
            // We need to add include/freerdp3 so that #include <freerdp/freerdp.h> works
            let mut paths = vec![include_path.clone()];
            if is_v3 {
                // FreeRDP 3.x: headers under freerdp3/ and winpr3/
                paths.push(freerdp3_dir);
                let winpr3_dir = include_path.join("winpr3");
                if winpr3_dir.exists() {
                    paths.push(winpr3_dir);
                }
                eprintln!("cargo:warning=Detected FreeRDP 3.x layout");
            } else {
                // FreeRDP 2.x: try freerdp2/winpr2 subdirs
                let freerdp2_dir = include_path.join("freerdp2");
                let winpr2_dir = include_path.join("winpr2");
                if freerdp2_dir.exists() {
                    paths.push(freerdp2_dir);
                }
                if winpr2_dir.exists() {
                    paths.push(winpr2_dir);
                }
            }
            return Some(paths);
        }
    }

    // Method 2: vcpkg (Windows)
    if let Ok(vcpkg_root) = env::var("VCPKG_ROOT").or_else(|_| env::var("VCPKG_INSTALLATION_ROOT"))
    {
        let vcpkg_include = PathBuf::from(&vcpkg_root)
            .join("installed")
            .join("x64-windows")
            .join("include");
        let vcpkg_lib = PathBuf::from(&vcpkg_root)
            .join("installed")
            .join("x64-windows")
            .join("lib");

        if vcpkg_include.exists() {
            // Detect FreeRDP version by checking for freerdp3 subdirectory
            let freerdp3_dir = vcpkg_include.join("freerdp3");
            let is_v3 = freerdp3_dir.exists();

            eprintln!(
                "cargo:warning=Found FreeRDP {} via vcpkg: {}",
                if is_v3 { "3.x" } else { "2.x" },
                vcpkg_include.display()
            );

            println!("cargo:rustc-link-search=native={}", vcpkg_lib.display());

            if is_v3 {
                // FreeRDP 3.x library names
                println!("cargo:rustc-link-lib=freerdp3");
                println!("cargo:rustc-link-lib=winpr3");
                println!("cargo:rustc-link-lib=freerdp-client3");
            } else {
                // FreeRDP 2.x library names
                println!("cargo:rustc-link-lib=freerdp2");
                println!("cargo:rustc-link-lib=winpr2");
            }

            // Build include paths
            let mut paths = vec![vcpkg_include.clone()];
            if is_v3 {
                // FreeRDP 3.x: headers under freerdp3/ and winpr3/
                paths.push(freerdp3_dir);
                let winpr3_dir = vcpkg_include.join("winpr3");
                if winpr3_dir.exists() {
                    paths.push(winpr3_dir);
                }
            }

            return Some(paths);
        }
    }

    // Method 3: pkg-config (Linux, macOS)
    // Try FreeRDP 3.x first (freerdp3), then fall back to 2.x (freerdp2)
    let freerdp_result = pkg_config::Config::new()
        .atleast_version("3.0")
        .probe("freerdp3")
        .or_else(|_| {
            pkg_config::Config::new()
                .atleast_version("2.0")
                .probe("freerdp2")
        });

    let winpr_result = pkg_config::Config::new()
        .atleast_version("3.0")
        .probe("winpr3")
        .or_else(|_| {
            pkg_config::Config::new()
                .atleast_version("2.0")
                .probe("winpr2")
        });

    if let (Ok(freerdp), Ok(winpr)) = (freerdp_result, winpr_result) {
        eprintln!("cargo:warning=Found FreeRDP via pkg-config");

        let mut paths: Vec<PathBuf> = freerdp.include_paths;
        paths.extend(winpr.include_paths);
        return Some(paths);
    }

    // Method 4: Common install locations
    let common_paths = [
        "/usr/include",
        "/usr/local/include",
        "/opt/homebrew/include",
    ];

    for path in common_paths {
        let include_path = PathBuf::from(path);
        // Try freerdp3 first, then freerdp (v2 or shared)
        let freerdp3_header = include_path
            .join("freerdp3")
            .join("freerdp")
            .join("freerdp.h");
        let freerdp_header = include_path.join("freerdp").join("freerdp.h");
        if freerdp3_header.exists() {
            eprintln!("cargo:warning=Found FreeRDP 3 in {}", path);
            // For FreeRDP 3, the headers are under freerdp3/ and winpr3/
            let mut paths = vec![include_path.clone()];
            let freerdp3_dir = include_path.join("freerdp3");
            let winpr3_dir = include_path.join("winpr3");
            if freerdp3_dir.exists() {
                paths.push(freerdp3_dir);
            }
            if winpr3_dir.exists() {
                paths.push(winpr3_dir);
            }
            return Some(paths);
        } else if freerdp_header.exists() {
            eprintln!("cargo:warning=Found FreeRDP in {}", path);
            return Some(vec![include_path]);
        }
    }

    None
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/ffi/wrapper.h");

    // Try to find FreeRDP using multiple methods
    let include_paths = match find_freerdp() {
        Some(paths) => paths,
        None => {
            eprintln!("cargo:warning=");
            eprintln!("cargo:warning=FreeRDP 3.x not found. Please install:");
            eprintln!("cargo:warning=  Ubuntu/Debian: apt install freerdp3-dev libwinpr3-dev");
            eprintln!("cargo:warning=  Fedora/RHEL:   dnf install freerdp-devel");
            eprintln!("cargo:warning=  macOS:         brew install freerdp");
            eprintln!("cargo:warning=  Windows:       vcpkg install freerdp:x64-windows");
            eprintln!("cargo:warning=");
            eprintln!("cargo:warning=Generating stub bindings for development...");

            generate_stub_bindings();
            return;
        }
    };

    // Generate bindings
    let clang_args: Vec<String> = include_paths
        .iter()
        .map(|p| format!("-I{}", p.display()))
        .collect();

    let bindings = bindgen::Builder::default()
        .header("src/ffi/wrapper.h")
        .clang_args(&clang_args)
        // FreeRDP core types
        .allowlist_type("freerdp")
        .allowlist_type("rdpContext")
        .allowlist_type("rdpSettings")
        .allowlist_type("rdpUpdate")
        .allowlist_type("rdpInput")
        .allowlist_type("rdpGraphics")
        .allowlist_type("rdpGdi")
        // FreeRDP functions
        .allowlist_function("freerdp_new")
        .allowlist_function("freerdp_free")
        .allowlist_function("freerdp_connect")
        .allowlist_function("freerdp_disconnect")
        .allowlist_function("freerdp_abort_connect")
        .allowlist_function("freerdp_shall_disconnect")
        .allowlist_function("freerdp_check_fds")
        .allowlist_function("freerdp_get_event_handles")
        .allowlist_function("freerdp_check_event_handles")
        .allowlist_function("freerdp_context_new")
        .allowlist_function("freerdp_context_free")
        .allowlist_function("freerdp_settings_new")
        .allowlist_function("freerdp_settings_free")
        .allowlist_function("freerdp_settings_get_.*")
        .allowlist_function("freerdp_settings_set_.*")
        // GDI functions
        .allowlist_function("gdi_init")
        .allowlist_function("gdi_free")
        .allowlist_function("gdi_get_.*")
        // Input functions
        .allowlist_function("freerdp_input_send_.*")
        // Clipboard functions
        .allowlist_type("CliprdrClient.*")
        .allowlist_function("cliprdr_.*")
        // WinPR types
        .allowlist_type("wLog")
        .allowlist_type("HANDLE")
        .allowlist_type("BOOL")
        .allowlist_type("UINT.*")
        .allowlist_type("INT.*")
        .allowlist_type("BYTE")
        .allowlist_type("DWORD")
        // Constants
        .allowlist_var("TRUE")
        .allowlist_var("FALSE")
        .allowlist_var("FREERDP_.*")
        .allowlist_var("FreeRDP_.*")
        // Derive traits
        .derive_debug(true)
        .derive_default(true)
        // Layout tests can be flaky on different platforms
        .layout_tests(false)
        // Generate bindings
        .generate()
        .expect("Unable to generate FreeRDP bindings");

    // Write bindings to OUT_DIR
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("freerdp_bindings.rs"))
        .expect("Couldn't write bindings!");
}

/// Generate stub bindings when FreeRDP is not available
///
/// This allows the crate to compile for documentation/development
/// but will panic at runtime if actually used.
fn generate_stub_bindings() {
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Stub bindings that match FreeRDP 3.x API
    let stub_bindings = r##"// Stub FreeRDP 3.x bindings - FreeRDP not installed
//
// These stubs allow the crate to compile without FreeRDP installed.
// Attempting to use these will panic at runtime.

#[allow(dead_code)]
#[allow(non_camel_case_types)]
#[allow(non_snake_case)]
#[allow(non_upper_case_globals)]
mod freerdp_stubs {
    use std::os::raw::{c_char, c_int, c_void};

// Basic types
pub type BOOL = c_int;
pub type BYTE = u8;
pub type UINT8 = u8;
pub type UINT16 = u16;
pub type UINT32 = u32;
pub type HANDLE = *mut c_void;

// FreeRDP 3.x typed settings keys
pub type FreeRDP_Settings_Keys_String = i32;
pub type FreeRDP_Settings_Keys_UInt32 = i32;
pub type FreeRDP_Settings_Keys_Bool = i32;

// String settings
pub const FreeRDP_Settings_Keys_String_FreeRDP_ServerHostname: FreeRDP_Settings_Keys_String = 20;
pub const FreeRDP_Settings_Keys_String_FreeRDP_Username: FreeRDP_Settings_Keys_String = 21;
pub const FreeRDP_Settings_Keys_String_FreeRDP_Password: FreeRDP_Settings_Keys_String = 22;
pub const FreeRDP_Settings_Keys_String_FreeRDP_Domain: FreeRDP_Settings_Keys_String = 23;

// UInt32 settings
pub const FreeRDP_Settings_Keys_UInt32_FreeRDP_ServerPort: FreeRDP_Settings_Keys_UInt32 = 19;
pub const FreeRDP_Settings_Keys_UInt32_FreeRDP_DesktopWidth: FreeRDP_Settings_Keys_UInt32 = 129;
pub const FreeRDP_Settings_Keys_UInt32_FreeRDP_DesktopHeight: FreeRDP_Settings_Keys_UInt32 = 130;
pub const FreeRDP_Settings_Keys_UInt32_FreeRDP_ColorDepth: FreeRDP_Settings_Keys_UInt32 = 131;
pub const FreeRDP_Settings_Keys_UInt32_FreeRDP_PerformanceFlags: FreeRDP_Settings_Keys_UInt32 = 960;
pub const FreeRDP_Settings_Keys_UInt32_FreeRDP_ConnectionType: FreeRDP_Settings_Keys_UInt32 = 969;

// Bool settings
pub const FreeRDP_Settings_Keys_Bool_FreeRDP_NlaSecurity: FreeRDP_Settings_Keys_Bool = 177;
pub const FreeRDP_Settings_Keys_Bool_FreeRDP_TlsSecurity: FreeRDP_Settings_Keys_Bool = 178;
pub const FreeRDP_Settings_Keys_Bool_FreeRDP_UseRdpSecurityLayer: FreeRDP_Settings_Keys_Bool = 192;
pub const FreeRDP_Settings_Keys_Bool_FreeRDP_ExtSecurity: FreeRDP_Settings_Keys_Bool = 179;
pub const FreeRDP_Settings_Keys_Bool_FreeRDP_IgnoreCertificate: FreeRDP_Settings_Keys_Bool = 1408;
pub const FreeRDP_Settings_Keys_Bool_FreeRDP_OldLicenseBehaviour: FreeRDP_Settings_Keys_Bool = 1606;
pub const FreeRDP_Settings_Keys_Bool_FreeRDP_SoftwareGdi: FreeRDP_Settings_Keys_Bool = 970;
pub const FreeRDP_Settings_Keys_Bool_FreeRDP_DisableWallpaper: FreeRDP_Settings_Keys_Bool = 962;
pub const FreeRDP_Settings_Keys_Bool_FreeRDP_DisableFullWindowDrag: FreeRDP_Settings_Keys_Bool = 963;
pub const FreeRDP_Settings_Keys_Bool_FreeRDP_DisableMenuAnims: FreeRDP_Settings_Keys_Bool = 964;
pub const FreeRDP_Settings_Keys_Bool_FreeRDP_DisableThemes: FreeRDP_Settings_Keys_Bool = 965;

// Opaque structs
#[repr(C)]
#[derive(Debug, Default)]
pub struct freerdp {
    pub context: *mut rdpContext,
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct rdpContext {
    pub instance: *mut freerdp,
    pub settings: *mut rdpSettings,
    pub input: *mut rdpInput,
    pub gdi: *mut rdpGdi,
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct rdpSettings {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct rdpInput {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct rdpGdi {
    pub width: c_int,
    pub height: c_int,
    pub primary: *mut gdiBitmap,
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct gdiBitmap {
    pub bitmap: *mut gdiBitmapData,
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct gdiBitmapData {
    pub data: *mut u8,
    pub scanline: c_int,
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct rdpUpdate {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct rdpGraphics {
    _private: [u8; 0],
}

// Stub functions - all panic with helpful message
fn stub_panic() -> ! {
    panic!(
        "FreeRDP 3.x is not installed. Please install FreeRDP development packages:\n\
         Ubuntu/Debian: apt install freerdp3-dev libwinpr3-dev\n\
         Fedora/RHEL:   dnf install freerdp-devel\n\
         macOS:         brew install freerdp"
    );
}

pub unsafe fn freerdp_new() -> *mut freerdp { stub_panic() }
pub unsafe fn freerdp_free(_instance: *mut freerdp) { stub_panic() }
pub unsafe fn freerdp_connect(_instance: *mut freerdp) -> BOOL { stub_panic() }
pub unsafe fn freerdp_disconnect(_instance: *mut freerdp) -> BOOL { stub_panic() }
pub unsafe fn freerdp_abort_connect(_instance: *mut freerdp) -> BOOL { stub_panic() }
pub unsafe fn freerdp_shall_disconnect(_instance: *mut freerdp) -> BOOL { stub_panic() }
pub unsafe fn freerdp_check_fds(_instance: *mut freerdp) -> BOOL { stub_panic() }
pub unsafe fn freerdp_check_event_handles(_context: *mut rdpContext) -> BOOL { stub_panic() }
pub unsafe fn freerdp_context_new(_instance: *mut freerdp) -> BOOL { stub_panic() }
pub unsafe fn freerdp_context_free(_instance: *mut freerdp) { stub_panic() }

pub unsafe fn freerdp_settings_set_string(
    _settings: *mut rdpSettings,
    _id: FreeRDP_Settings_Keys_String,
    _value: *const c_char,
) -> BOOL { stub_panic() }

pub unsafe fn freerdp_settings_set_uint32(
    _settings: *mut rdpSettings,
    _id: FreeRDP_Settings_Keys_UInt32,
    _value: UINT32,
) -> BOOL { stub_panic() }

pub unsafe fn freerdp_settings_set_bool(
    _settings: *mut rdpSettings,
    _id: FreeRDP_Settings_Keys_Bool,
    _value: BOOL,
) -> BOOL { stub_panic() }

pub unsafe fn freerdp_input_send_keyboard_event(
    _input: *mut rdpInput,
    _flags: UINT16,
    _code: UINT8,
) -> BOOL { stub_panic() }

pub unsafe fn freerdp_input_send_mouse_event(
    _input: *mut rdpInput,
    _flags: UINT16,
    _x: UINT16,
    _y: UINT16,
) -> BOOL { stub_panic() }
}

pub use freerdp_stubs::*;
"##;

    fs::write(out_path.join("freerdp_bindings.rs"), stub_bindings)
        .expect("Failed to write stub bindings");
}
