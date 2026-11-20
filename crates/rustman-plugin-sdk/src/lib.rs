//! Guest SDK for Rustman plugins (add-plugin-system design D8).
//!
//! A plugin is a WebAssembly component that implements one or more worlds
//! of the `rustman:plugin` WIT package shipped in this crate's `wit/`
//! folder. The SDK is versioned with the plugin API major, so plugins do
//! not depend on the Rustman workspace.
//!
//! Each extension point is a cargo feature (`auth`, `assertion`,
//! `generator`, `transformer`, `importer`, `kind`, `report`) that turns on
//! the `wit-bindgen` bindings of its world: a module of the same name with
//! the `Guest` trait to implement and an `export!` macro. The host imports
//! are in `host`. The `runtime` feature adds the allocator and panic
//! handler of a `no_std` plugin, which then imports nothing but the host
//! interfaces: the host links no WASI at all.
//!
//! ```ignore
//! #![no_std]
//! extern crate alloc;
//! use alloc::{string::String, vec::Vec};
//! use rustman_plugin_sdk::generator::{Guest, export};
//! struct Plugin;
//! impl Guest for Plugin {
//!     fn names() -> Vec<String> { /* ... */ }
//!     fn generate(name: String, args: Vec<String>) -> Result<String, String> { /* ... */ }
//! }
//! export!(Plugin);
//! ```
//!
//! `rustman plugin new <name> --kind <extension point>` scaffolds a
//! project that builds with `cargo build --target wasm32-wasip2 --release`.

#![cfg_attr(not(test), no_std)]

#[cfg(any(feature = "bindings", feature = "runtime"))]
extern crate alloc;

/// The WIT package name.
pub const PACKAGE: &str = "rustman:plugin";

/// The plugin API version this SDK targets. A plugin's manifest declares
/// its major as `api`.
pub const API_VERSION: &str = "1.0.0";

/// The plugin API major of [`API_VERSION`].
pub const API_MAJOR: u32 = 1;

/// The WIT source of the package.
pub const WIT: &str = include_str!("../wit/plugin.wit");

/// The worlds of the package, one per extension point, with the interface
/// each exports.
pub const WORLDS: [(&str, &str); 7] = [
    ("auth", "auth-scheme"),
    ("assertion", "assertion-fn"),
    ("generator", "value-generator"),
    ("transformer", "body-transformer"),
    ("importer", "format-importer"),
    ("kind", "request-kind"),
    ("report", "report-renderer"),
];

/// The host imports every plugin can call: `http`, `clock`, `random`,
/// `env`, `fs-read`, `secrets` and `log` under `rustman::plugin`. Each
/// returns `capability-denied` without its grant.
#[cfg(feature = "bindings")]
pub mod host {
    wit_bindgen::generate!({
        world: "rustman:plugin/host",
        path: "wit/plugin.wit",
    });
}

#[cfg(feature = "bindings")]
pub use host::rustman::plugin::{clock, env, fs_read, http, log, random, secrets, types};

/// A host error as the host itself words it: `capability_denied: clock`,
/// `policy_denied: ...`, or the failure's message.
#[cfg(feature = "bindings")]
pub fn describe(error: &types::HostError) -> alloc::string::String {
    use alloc::format;
    match error {
        types::HostError::CapabilityDenied(detail) => format!("capability_denied: {detail}"),
        types::HostError::PolicyDenied(detail) => format!("policy_denied: {detail}"),
        types::HostError::Failed(detail) => detail.clone(),
    }
}

/// The bindings of one extension point world, reusing [`host`]'s imports.
#[cfg(feature = "bindings")]
macro_rules! world {
    ($feature:literal, $module:ident, $world:literal, $path:literal, $export:ident) => {
        #[cfg(feature = $feature)]
        pub mod $module {
            pub use self::exports::rustman::plugin::$export::*;

            wit_bindgen::generate!({
                world: $world,
                path: "wit/plugin.wit",
                with: {
                    "rustman:plugin/types@1.0.0": crate::host::rustman::plugin::types,
                    "rustman:plugin/http@1.0.0": crate::host::rustman::plugin::http,
                    "rustman:plugin/clock@1.0.0": crate::host::rustman::plugin::clock,
                    "rustman:plugin/random@1.0.0": crate::host::rustman::plugin::random,
                    "rustman:plugin/env@1.0.0": crate::host::rustman::plugin::env,
                    "rustman:plugin/fs-read@1.0.0": crate::host::rustman::plugin::fs_read,
                    "rustman:plugin/secrets@1.0.0": crate::host::rustman::plugin::secrets,
                    "rustman:plugin/log@1.0.0": crate::host::rustman::plugin::log,
                },
                pub_export_macro: true,
                default_bindings_module: $path,
            });
        }
    };
}

#[cfg(feature = "bindings")]
world!(
    "auth",
    auth,
    "rustman:plugin/auth",
    "rustman_plugin_sdk::auth",
    auth_scheme
);
#[cfg(feature = "bindings")]
world!(
    "assertion",
    assertion,
    "rustman:plugin/assertion",
    "rustman_plugin_sdk::assertion",
    assertion_fn
);
#[cfg(feature = "bindings")]
world!(
    "generator",
    generator,
    "rustman:plugin/generator",
    "rustman_plugin_sdk::generator",
    value_generator
);
#[cfg(feature = "bindings")]
world!(
    "transformer",
    transformer,
    "rustman:plugin/transformer",
    "rustman_plugin_sdk::transformer",
    body_transformer
);
#[cfg(feature = "bindings")]
world!(
    "importer",
    importer,
    "rustman:plugin/importer",
    "rustman_plugin_sdk::importer",
    format_importer
);
#[cfg(feature = "bindings")]
world!(
    "kind",
    kind,
    "rustman:plugin/kind",
    "rustman_plugin_sdk::kind",
    request_kind
);
#[cfg(feature = "bindings")]
world!(
    "report",
    report,
    "rustman:plugin/report",
    "rustman_plugin_sdk::report",
    report_renderer
);

/// The allocator, `cabi_realloc` and panic handler of a `no_std` plugin
/// component (`wasm32-wasip2` leaves `cabi_realloc` to the C library, which
/// a `no_std` plugin does not link). A panic traps, which the host reports
/// as `plugin <name> trapped: ...` and recovers from by recreating the
/// instance.
#[cfg(all(feature = "runtime", target_arch = "wasm32", not(test)))]
mod runtime {
    use alloc::alloc::{Layout, alloc, realloc};

    #[global_allocator]
    static ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

    #[panic_handler]
    fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
        core::arch::wasm32::unreachable()
    }

    /// The canonical ABI's allocation function, which the host calls to
    /// pass strings and lists into the plugin.
    ///
    /// # Safety
    ///
    /// Called by the host with a valid previous allocation (or none) and a
    /// power-of-two alignment, as the canonical ABI requires.
    #[unsafe(export_name = "cabi_realloc")]
    unsafe extern "C" fn cabi_realloc(
        old_ptr: *mut u8,
        old_len: usize,
        align: usize,
        new_len: usize,
    ) -> *mut u8 {
        if new_len == 0 {
            return align as *mut u8;
        }
        let ptr = unsafe {
            if old_len == 0 {
                alloc(Layout::from_size_align_unchecked(new_len, align))
            } else {
                let layout = Layout::from_size_align_unchecked(old_len, align);
                realloc(old_ptr, layout, new_len)
            }
        };
        if ptr.is_null() {
            core::arch::wasm32::unreachable();
        }
        ptr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wit_declares_the_package_version_and_every_world() {
        let header = format!("package {PACKAGE}@{API_VERSION};");
        assert!(WIT.contains(&header), "{header}");
        assert!(API_VERSION.starts_with(&format!("{API_MAJOR}.")));
        for (world, export) in WORLDS {
            assert!(WIT.contains(&format!("world {world} {{")), "{world}");
            assert!(WIT.contains(&format!("export {export};")), "{export}");
        }
    }
}
