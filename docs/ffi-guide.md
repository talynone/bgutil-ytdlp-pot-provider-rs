# FFI (Foreign Function Interface) Guide

This guide explains how to compile and use `bgutil-ytdlp-pot-provider` as a C dynamic library (`cdylib`) for integration with other programming languages.

## Overview

The FFI module exposes two C-compatible functions:

| Function | Description |
|----------|-------------|
| `ffi_generate` | Generate a POT token. Returns a JSON string. |
| `ffi_free_string` | Free a string returned by `ffi_generate`. |

## Building

### Compile as a Dynamic Library

The FFI module is gated behind the `ffi` feature flag. Enable it when building:

```bash
cargo build --release --features ffi
```

This produces both a static library and a dynamic library:

- **Linux**: `target/release/libbgutil_ytdlp_pot_provider.so`
- **macOS**: `target/release/libbgutil_ytdlp_pot_provider.dylib`
- **Windows**: `target/release/bgutil_ytdlp_pot_provider.dll`

> **Note**: The regular binary (`bgutil-pot`) is still built alongside the library. The `ffi` feature only adds the FFI exports to the shared library; it does not affect the CLI binary.

## API Reference

### `ffi_generate`

```c
char* ffi_generate(
    const char* content_binding,  // Video ID, visitor data, etc. (nullable)
    const char* proxy,            // Proxy URL, e.g. "http://host:port" (nullable)
    bool bypass_cache,            // true to force new token generation
    const char* source_address,   // Source IP for outbound connections (nullable)
    bool disable_tls              // true to disable TLS verification
);
```

**Returns**: A pointer to a null-terminated JSON string. Ownership is transferred to the caller.

**Success response**:

```json
{
  "poToken": "Ehz0BE...",
  "contentBinding": "VIDEO_ID",
  "expiresAt": "2026-03-11T19:39:27.000000000Z"
}
```

**Error response**:

```json
{
  "error": "description of what went wrong"
}
```

### `ffi_free_string`

```c
void ffi_free_string(char* ptr);
```

Frees a string previously returned by `ffi_generate`. Passing `NULL` is safe (no-op).

**Important**: Each pointer must be freed **exactly once**. Double-free is undefined behavior.

## Safety Contract

1. **No panics across FFI boundary**: All errors are returned as JSON error objects.
2. **No `process::exit()`**: Unlike some FFI implementations, this module never terminates the host process. Errors are always communicated through the return value.
3. **Invalid UTF-8 handling**: If a C string parameter contains invalid UTF-8 bytes, an error JSON is returned instead of panicking.
4. **Thread safety**: The global Tokio runtime is lazily initialized and shared. Multiple threads can call `ffi_generate` concurrently.

## Language-Specific Examples

### Java (JNA)

```java
import com.sun.jna.Library;
import com.sun.jna.Native;
import com.sun.jna.Pointer;

public interface BGUtil extends Library {
    BGUtil INSTANCE = Native.load("bgutil_ytdlp_pot_provider", BGUtil.class);

    Pointer ffi_generate(
        String content_binding,
        String proxy,
        boolean bypass_cache,
        String source_address,
        boolean disable_tls
    );

    void ffi_free_string(Pointer ptr);

    static String generate(String contentBinding) {
        Pointer ptr = INSTANCE.ffi_generate(contentBinding, null, false, null, false);
        try {
            return ptr.getString(0);
        } finally {
            INSTANCE.ffi_free_string(ptr);
        }
    }
}
```

### Python (ctypes)

```python
import ctypes
import json

lib = ctypes.CDLL("./libbgutil_ytdlp_pot_provider.so")

lib.ffi_generate.restype = ctypes.c_void_p
lib.ffi_generate.argtypes = [
    ctypes.c_char_p,  # content_binding
    ctypes.c_char_p,  # proxy
    ctypes.c_bool,    # bypass_cache
    ctypes.c_char_p,  # source_address
    ctypes.c_bool,    # disable_tls
]

lib.ffi_free_string.restype = None
lib.ffi_free_string.argtypes = [ctypes.c_void_p]

def generate_pot(content_binding: str) -> dict:
    ptr = lib.ffi_generate(
        content_binding.encode("utf-8"),
        None,   # no proxy
        False,  # use cache
        None,   # no source address
        False,  # verify TLS
    )
    try:
        result = ctypes.cast(ptr, ctypes.c_char_p).value.decode("utf-8")
        return json.loads(result)
    finally:
        lib.ffi_free_string(ptr)

# Usage
result = generate_pot("VIDEO_ID")
if "error" in result:
    print(f"Error: {result['error']}")
else:
    print(f"POT Token: {result['poToken']}")
```

### C# (P/Invoke)

```csharp
using System;
using System.Runtime.InteropServices;
using System.Text.Json;

public static class BGUtil
{
    private const string LibName = "bgutil_ytdlp_pot_provider";

    [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr ffi_generate(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? contentBinding,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? proxy,
        [MarshalAs(UnmanagedType.I1)] bool bypassCache,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? sourceAddress,
        [MarshalAs(UnmanagedType.I1)] bool disableTls
    );

    [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
    private static extern void ffi_free_string(IntPtr ptr);

    public static string Generate(string? contentBinding)
    {
        IntPtr ptr = ffi_generate(contentBinding, null, false, null, false);
        try
        {
            return Marshal.PtrToStringUTF8(ptr) ?? "{}";
        }
        finally
        {
            ffi_free_string(ptr);
        }
    }
}
```

### Go (cgo)

```go
package main

/*
#cgo LDFLAGS: -L. -lbgutil_ytdlp_pot_provider
#include <stdlib.h>

extern char* ffi_generate(
    const char* content_binding,
    const char* proxy,
    int bypass_cache,
    const char* source_address,
    int disable_tls
);
extern void ffi_free_string(char* ptr);
*/
import "C"

import (
    "encoding/json"
    "fmt"
    "unsafe"
)

func GeneratePOT(contentBinding string) (map[string]interface{}, error) {
    cb := C.CString(contentBinding)
    defer C.free(unsafe.Pointer(cb))

    ptr := C.ffi_generate(cb, nil, 0, nil, 0)
    defer C.ffi_free_string(ptr)

    result := C.GoString(ptr)
    var parsed map[string]interface{}
    err := json.Unmarshal([]byte(result), &parsed)
    return parsed, err
}

func main() {
    result, err := GeneratePOT("VIDEO_ID")
    if err != nil {
        panic(err)
    }
    fmt.Println(result)
}
```

## Error Handling

The FFI layer **never** panics or calls `process::exit()`. All errors are returned as JSON objects with an `"error"` field. Callers should always check for this field before accessing `"poToken"`.

Common error scenarios:

| Error | Cause |
|-------|-------|
| `"Invalid UTF-8 in C string"` | A parameter contains non-UTF-8 bytes |
| `"Failed to get cache path"` | Cannot determine the cache directory |
| `"POT generation failed: ..."` | BotGuard or network error during generation |
| `"Failed to serialize response"` | Internal serialization error (rare) |

## Feature Flag

The FFI module is conditionally compiled behind the `ffi` feature flag. This means:

- **Without `--features ffi`**: No FFI symbols are exported. The shared library is still built (due to the `cdylib` crate type in `Cargo.toml`), but it contains no useful FFI entry points.
- **With `--features ffi`**: The `ffi_generate` and `ffi_free_string` symbols are exported and available for foreign language binding.

This design ensures the FFI code has zero impact on the regular binary build.
