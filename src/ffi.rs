//! Foreign Function Interface (FFI) for multi-language integration
//!
//! This module provides a C-compatible FFI layer that allows other programming languages
//! (Java, Python, C#, Go, etc.) to use the POT token generation functionality by compiling
//! this crate as a `cdylib`.
//!
//! # Safety
//!
//! All FFI functions follow these safety conventions:
//! - Null pointers are handled gracefully (treated as `None` for optional parameters)
//! - Invalid UTF-8 strings return an error JSON instead of panicking
//! - The returned `*mut c_char` must be freed by calling [`ffi_free_string`]
//! - No `process::exit()` calls — errors are returned as JSON error objects
//!
//! # Thread Safety
//!
//! A global Tokio runtime is lazily initialized on first use. All FFI calls share
//! this runtime and are safe to call from multiple threads.
//!
//! # Example (C)
//!
//! ```c
//! #include <stdio.h>
//!
//! // Declarations matching the exported FFI symbols
//! extern char* ffi_generate(
//!     const char* content_binding,
//!     const char* proxy,
//!     int bypass_cache,
//!     const char* source_address,
//!     int disable_tls
//! );
//! extern void ffi_free_string(char* ptr);
//!
//! int main() {
//!     char* result = ffi_generate("VIDEO_ID", NULL, 0, NULL, 0);
//!     printf("Result: %s\n", result);
//!     ffi_free_string(result);
//!     return 0;
//! }
//! ```

use std::ffi::{CStr, CString, c_char};
use std::sync::OnceLock;

use tokio::runtime::Runtime;
use tracing::{debug, warn};

use crate::types::PotRequest;
use crate::utils::cache::{FileCache, get_cache_path};
use crate::{SessionManager, Settings};

/// Global Tokio runtime shared across all FFI calls.
///
/// Lazily initialized on first use via [`get_runtime`]. Using `OnceLock` ensures
/// thread-safe one-time initialization without the overhead of a mutex on every call.
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Returns a reference to the global Tokio runtime, initializing it on first call.
///
/// # Panics
///
/// Panics if the Tokio runtime cannot be created (e.g., due to OS resource exhaustion).
/// This is intentional — if we can't create an async runtime, there is no meaningful
/// way to proceed.
fn get_runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| Runtime::new().expect("Failed to create Tokio runtime for FFI"))
}

/// Safely convert a nullable C string pointer to an `Option<String>`.
///
/// Returns `None` if the pointer is null. Returns `Err` if the pointer contains
/// invalid UTF-8 data.
fn optional_cstr(ptr: *const c_char) -> Result<Option<String>, String> {
    if ptr.is_null() {
        return Ok(None);
    }
    // SAFETY: We checked for null above. The caller is responsible for ensuring the
    // pointer is valid and null-terminated, as documented in the FFI contract.
    let c_str = unsafe { CStr::from_ptr(ptr) };
    c_str
        .to_str()
        .map(|s| Some(s.to_string()))
        .map_err(|e| format!("Invalid UTF-8 in C string: {}", e))
}

/// Create a JSON error string to return across the FFI boundary.
fn make_error_json(error: &str) -> *mut c_char {
    let json = serde_json::json!({ "error": error });
    let s = serde_json::to_string(&json)
        .unwrap_or_else(|_| r#"{"error":"serialization failed"}"#.to_string());
    CString::new(s)
        .unwrap_or_else(|_| CString::new(r#"{"error":"null byte in error message"}"#).unwrap())
        .into_raw()
}

/// Build a [`PotRequest`] from FFI parameters.
fn build_ffi_pot_request(
    content_binding: Option<String>,
    proxy: Option<String>,
    bypass_cache: bool,
    source_address: Option<String>,
    disable_tls: bool,
) -> PotRequest {
    let mut request = PotRequest::new();

    if let Some(cb) = content_binding {
        request = request.with_content_binding(cb);
    }

    if let Some(p) = proxy {
        request = request.with_proxy(p);
    }

    if bypass_cache {
        request = request.with_bypass_cache(true);
    }

    if let Some(sa) = source_address {
        request = request.with_source_address(sa);
    }

    if disable_tls {
        request = request.with_disable_tls_verification(true);
    }

    // Force disable Innertube for FFI mode (matching script mode behavior)
    request = request.with_disable_innertube(true);

    request
}

/// Generate a POT token and return the result as a JSON string.
///
/// This is the primary FFI entry point. It initializes caching, creates a session manager,
/// generates a POT token, and returns the result as a JSON-encoded C string.
///
/// # Parameters
///
/// - `content_binding`: Content binding string (video ID, visitor data, etc.). May be null.
/// - `proxy`: Proxy URL string (e.g., `http://host:port`). May be null for no proxy.
/// - `bypass_cache`: If non-zero (`true`), bypasses the cache and forces new token generation.
/// - `source_address`: Source IP address for outbound connections. May be null.
/// - `disable_tls`: If non-zero (`true`), disables TLS certificate verification.
///
/// # Returns
///
/// A pointer to a null-terminated JSON string. On success, the JSON contains:
/// ```json
/// {
///   "poToken": "...",
///   "contentBinding": "...",
///   "expiresAt": "2026-03-11T13:39:27.000000000Z"
/// }
/// ```
///
/// On error, the JSON contains:
/// ```json
/// {
///   "error": "description of what went wrong"
/// }
/// ```
///
/// # Safety
///
/// - All pointer parameters must be either null or valid pointers to null-terminated C strings.
/// - The returned pointer **must** be freed by calling [`ffi_free_string`]. Failure to do so
///   will leak memory.
/// - This function does **not** call `std::process::exit()` — errors are always returned
///   as JSON error objects.
///
/// # Thread Safety
///
/// This function is safe to call from multiple threads concurrently.
#[unsafe(no_mangle)]
pub extern "C" fn ffi_generate(
    content_binding: *const c_char,
    proxy: *const c_char,
    bypass_cache: bool,
    source_address: *const c_char,
    disable_tls: bool,
) -> *mut c_char {
    // Parse C string parameters with proper error handling (no panics)
    let content_binding = match optional_cstr(content_binding) {
        Ok(v) => v,
        Err(e) => return make_error_json(&format!("content_binding: {}", e)),
    };

    let proxy = match optional_cstr(proxy) {
        Ok(v) => v,
        Err(e) => return make_error_json(&format!("proxy: {}", e)),
    };

    let source_address = match optional_cstr(source_address) {
        Ok(v) => v,
        Err(e) => return make_error_json(&format!("source_address: {}", e)),
    };

    let request = build_ffi_pot_request(
        content_binding,
        proxy,
        bypass_cache,
        source_address,
        disable_tls,
    );

    debug!(
        "FFI: Starting POT generation with parameters: content_binding={:?}, proxy={:?}, bypass_cache={}",
        request.content_binding,
        request.proxy,
        request.bypass_cache.unwrap_or(false)
    );

    let mut output = String::from("{}");

    get_runtime().block_on(async {
        // Initialize file cache
        let cache_path = match get_cache_path() {
            Ok(v) => v,
            Err(e) => {
                output = serde_json::json!({ "error": format!("Failed to get cache path: {}", e) }).to_string();
                return;
            }
        };
        let file_cache = FileCache::new(cache_path);

        // Load existing cache
        let session_data_caches = file_cache.load_cache().await.unwrap_or_else(|e| {
            warn!("Failed to load cache: {}. Starting with empty cache.", e);
            std::collections::HashMap::new()
        });

        // Initialize session manager with cache
        let settings = Settings::default();
        let session_manager = SessionManager::new(settings);
        session_manager
            .set_session_data_caches(session_data_caches)
            .await;

        // Generate POT token
        match session_manager.generate_pot_token(&request).await {
            Ok(response) => {
                // Save updated cache
                if let Err(e) = file_cache
                    .save_cache(session_manager.get_session_data_caches(true).await)
                    .await
                {
                    warn!("Failed to save cache: {}", e);
                }

                // Serialize result as JSON
                output = serde_json::to_string(&response).unwrap_or_else(|e| {
                    serde_json::json!({ "error": format!("Failed to serialize response: {}", e) }).to_string()
                });

                // Shutdown session manager to properly cleanup V8 isolates
                session_manager.shutdown().await;
            }
            Err(e) => {
                // Shutdown session manager before returning error
                session_manager.shutdown().await;

                output = serde_json::json!({ "error": format!("POT generation failed: {}", e) }).to_string();
            }
        }
    });

    // Transfer ownership of the string to the caller
    CString::new(output)
        .unwrap_or_else(|_| CString::new(r#"{"error":"null byte in output"}"#).unwrap())
        .into_raw()
}

/// Free a string previously returned by [`ffi_generate`].
///
/// # Safety
///
/// - `ptr` must be a pointer that was previously returned by [`ffi_generate`],
///   or null (in which case this function is a no-op).
/// - Each pointer must be freed **exactly once**. Double-free is undefined behavior.
/// - After calling this function, the pointer is invalid and must not be dereferenced.
#[unsafe(no_mangle)]
pub extern "C" fn ffi_free_string(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: The caller guarantees this pointer was obtained from `CString::into_raw()`
    // via `ffi_generate`, and has not been freed before.
    unsafe {
        let _ = CString::from_raw(ptr); // Dropped here, memory freed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn test_optional_cstr_null() {
        let result = optional_cstr(std::ptr::null());
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn test_optional_cstr_valid() {
        let c_string = CString::new("hello").unwrap();
        let result = optional_cstr(c_string.as_ptr());
        assert_eq!(result.unwrap(), Some("hello".to_string()));
    }

    #[test]
    fn test_optional_cstr_invalid_utf8() {
        // Create a CString with invalid UTF-8 bytes
        let bytes: Vec<u8> = vec![0xff, 0xfe, 0x00]; // null-terminated invalid UTF-8
        let ptr = bytes.as_ptr() as *const c_char;
        let result = optional_cstr(ptr);
        assert!(result.is_err());
        std::mem::forget(bytes); // Don't drop the backing memory
    }

    #[test]
    fn test_make_error_json() {
        let ptr = make_error_json("test error");
        assert!(!ptr.is_null());
        let c_str = unsafe { CStr::from_ptr(ptr) };
        let s = c_str.to_str().unwrap();
        assert!(s.contains("error"));
        assert!(s.contains("test error"));
        // Free the string
        ffi_free_string(ptr);
    }

    #[test]
    fn test_build_ffi_pot_request_all_none() {
        let request = build_ffi_pot_request(None, None, false, None, false);
        assert_eq!(request.content_binding, None);
        assert_eq!(request.proxy, None);
        assert_eq!(request.bypass_cache, Some(false));
        assert_eq!(request.source_address, None);
        assert_eq!(request.disable_tls_verification, Some(false));
        assert_eq!(request.disable_innertube, Some(true));
    }

    #[test]
    fn test_build_ffi_pot_request_all_set() {
        let request = build_ffi_pot_request(
            Some("video_id".to_string()),
            Some("http://proxy:8080".to_string()),
            true,
            Some("192.168.1.1".to_string()),
            true,
        );
        assert_eq!(request.content_binding, Some("video_id".to_string()));
        assert_eq!(request.proxy, Some("http://proxy:8080".to_string()));
        assert_eq!(request.bypass_cache, Some(true));
        assert_eq!(request.source_address, Some("192.168.1.1".to_string()));
        assert_eq!(request.disable_tls_verification, Some(true));
        assert_eq!(request.disable_innertube, Some(true));
    }

    #[test]
    fn test_ffi_free_string_null() {
        // Should not panic
        ffi_free_string(std::ptr::null_mut());
    }

    #[test]
    fn test_ffi_generate_with_null_params() {
        // Should return valid JSON (either success or error), never panic
        let result = ffi_generate(
            std::ptr::null(),
            std::ptr::null(),
            false,
            std::ptr::null(),
            false,
        );
        assert!(!result.is_null());

        let c_str = unsafe { CStr::from_ptr(result) };
        let s = c_str.to_str().unwrap();
        // Should be valid JSON
        let parsed: serde_json::Value = serde_json::from_str(s).unwrap();
        assert!(parsed.is_object());

        ffi_free_string(result);
    }

    #[test]
    fn test_ffi_generate_with_content_binding() {
        let content_binding = CString::new("test_video_id").unwrap();
        let result = ffi_generate(
            content_binding.as_ptr(),
            std::ptr::null(),
            false,
            std::ptr::null(),
            false,
        );
        assert!(!result.is_null());

        let c_str = unsafe { CStr::from_ptr(result) };
        let s = c_str.to_str().unwrap();
        // Should be valid JSON
        let parsed: serde_json::Value = serde_json::from_str(s).unwrap();
        assert!(parsed.is_object());

        ffi_free_string(result);
    }
}
