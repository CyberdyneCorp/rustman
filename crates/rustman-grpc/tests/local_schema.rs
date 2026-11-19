//! Local schema sources (add-grpc-client task 2.2): `.proto` files compiled
//! in-process with import paths, compile errors with `file:line:col`,
//! workspace containment, and descriptor sets.

mod common;

use common::Fixture;
use rustman_grpc::{
    CachePolicy, EXIT_CONFIG, Remote, SchemaOrigin, SchemaSource, compile_protos, load_schema,
};
use rustman_test_support::grpc::descriptor_set;

const HEAD: &str = "version: 1\nkind: grpc\nname: r\ntarget: grpc://127.0.0.1:1\n";

/// A target nothing listens on: local sources never connect.
fn nowhere() -> Remote {
    Remote::parse("grpc://127.0.0.1:1").unwrap()
}

/// Spec "Compile proto files", with no `protoc` involved.
#[tokio::test(flavor = "multi_thread")]
async fn proto_files_compile_with_import_paths() {
    let f = Fixture::new();
    f.copy_protos("grpc/protos");
    f.write(
        "grpc/get.rman.yaml",
        &format!(
            "{HEAD}method: shop.v1.Catalog/GetProduct\nmessage: {{id: p-1}}\nschema:\n  protos: [protos/shop/v1/catalog.proto]\n  import_paths: [protos]\n"
        ),
    );
    let resolved = f.resolve("r").unwrap();
    let source = SchemaSource::of(&resolved.schema);
    assert!(matches!(source, SchemaSource::Protos { .. }));
    let schema = load_schema(&source, &nowhere(), &CachePolicy::default())
        .await
        .unwrap();
    assert_eq!(schema.origin, SchemaOrigin::Protos);
    let method = schema.method(&resolved.method).unwrap();
    assert_eq!(method.input().full_name(), "shop.v1.GetProductRequest");
    assert_eq!(method.output().full_name(), "shop.v1.Product");
    assert!(schema.pool.get_message_by_name("shop.v1.Money").is_some());
}

/// Spec "Proto syntax error": a missing semicolon on line 7.
#[test]
fn syntax_errors_name_file_line_and_column() {
    let f = Fixture::new();
    let text = "syntax = \"proto3\";\n\npackage shop.v1;\n\nmessage GetProductRequest {\n  string id = 1;\n  string name = 2\n}\n";
    f.write("protos/catalog.proto", text);
    let protos = [f.root().join("protos/catalog.proto")];
    let err = compile_protos(&protos, &[f.root().join("protos")]).unwrap_err();
    let message = err.to_string();
    assert!(message.starts_with("catalog.proto:7:"), "{message}");
    assert!(message.contains("';'"), "{message}");
    assert_eq!(err.exit_code(), EXIT_CONFIG);
}

#[test]
fn type_errors_and_missing_imports_name_the_file() {
    let f = Fixture::new();
    f.write(
        "protos/a.proto",
        "syntax = \"proto3\";\npackage a;\nmessage A {\n  Missing m = 1;\n}\n",
    );
    let err = compile_protos(&[f.root().join("protos/a.proto")], &[]).unwrap_err();
    assert!(err.to_string().starts_with("a.proto:4:"), "{err}");
    f.write(
        "protos/b.proto",
        "syntax = \"proto3\";\nimport \"nope.proto\";\npackage b;\n",
    );
    let err = compile_protos(&[f.root().join("protos/b.proto")], &[]).unwrap_err();
    assert!(err.to_string().starts_with("b.proto:2:"), "{err}");
    assert!(err.to_string().contains("nope.proto"), "{err}");
}

/// Schema paths are resolved against the request file and may not climb
/// out of the workspace.
#[test]
fn schema_paths_cannot_escape_the_workspace() {
    let f = Fixture::new();
    f.write(
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n[variables]\nup = \"../..\"\n",
    );
    f.write(
        "grpc/r.rman.yaml",
        &format!(
            "{HEAD}method: a.S/M\nmessage: {{}}\nschema:\n  protos: ['{{{{up}}}}/outside.proto']\n"
        ),
    );
    let err = f.resolve("r").unwrap_err();
    assert!(
        err.to_string().starts_with("file path escapes workspace"),
        "{err}"
    );
    f.write(
        "grpc/r.rman.yaml",
        &format!("{HEAD}method: a.S/M\nmessage: {{}}\nschema:\n  protos: [../../outside.proto]\n"),
    );
    let err = match rustman_core::Workspace::load(f.root()) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("the escaping path loads"),
    };
    assert!(err.contains("file path escapes workspace"), "{err}");
}

/// An import that climbs out of its import path, with `..` or an absolute
/// path, is refused with a message that says so; nothing outside is read.
#[test]
fn imports_that_climb_out_are_refused_clearly() {
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.proto");
    std::fs::write(
        &secret,
        "syntax = \"proto3\";\nmessage OUTSIDE_SECRET_MARKER {}\n",
    )
    .unwrap();
    let absolute = secret.to_string_lossy().replace('\\', "/");
    for import in ["sub/../../outside/secret.proto", absolute.as_str()] {
        let f = Fixture::new();
        f.write(
            "protos/m.proto",
            &format!("syntax = \"proto3\";\npackage m;\nimport \"{import}\";\n"),
        );
        let protos = [f.root().join("protos/m.proto")];
        let err = compile_protos(&protos, &[f.root().join("protos")]).unwrap_err();
        let message = err.to_string();
        // protox refuses the path itself; a Windows drive path (`C:/...`)
        // passes its check and is refused by the containment check.
        let refused = message.contains("m.proto:3:8: invalid import path:")
            && message.contains("an import cannot leave its import path");
        let contained = message.contains("resolves outside its import path");
        assert!(refused || (cfg!(windows) && contained), "{message}");
        assert!(!message.contains("group name"), "{message}");
        assert!(!message.contains("OUTSIDE_SECRET_MARKER"), "{message}");
        assert_eq!(err.exit_code(), EXIT_CONFIG);
    }
}

/// An import reached through a symlink that points out of its import path
/// is refused, so a proto outside the workspace is never read.
#[cfg(unix)]
#[test]
fn imports_through_symlinks_cannot_escape() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        outside.path().join("secret.proto"),
        "syntax = \"proto3\";\npackage leak;\nmessage OUTSIDE_SECRET_MARKER {\n  string x = 1;\n}\n",
    )
    .unwrap();
    let f = Fixture::new();
    f.write(
        "protos/sym.proto",
        "syntax = \"proto3\";\nimport \"link.proto\";\npackage s;\n",
    );
    std::os::unix::fs::symlink(
        outside.path().join("secret.proto"),
        f.root().join("protos/link.proto"),
    )
    .unwrap();
    let protos = [f.root().join("protos/sym.proto")];
    let err = compile_protos(&protos, &[f.root().join("protos")]).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("import 'link.proto' resolves outside its import path"),
        "{message}"
    );
    assert!(!message.contains("OUTSIDE_SECRET_MARKER"), "{message}");
    assert_eq!(err.exit_code(), EXIT_CONFIG);

    // A symlink to a file inside the import path still works.
    f.write(
        "protos/real.proto",
        "syntax = \"proto3\";\npackage r;\nmessage Inside {}\n",
    );
    std::fs::remove_file(f.root().join("protos/link.proto")).unwrap();
    std::os::unix::fs::symlink(
        f.root().join("protos/real.proto"),
        f.root().join("protos/link.proto"),
    )
    .unwrap();
    let pool = compile_protos(&protos, &[f.root().join("protos")]).unwrap();
    assert!(pool.get_message_by_name("r.Inside").is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn descriptor_sets_load() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.root().join("grpc")).unwrap();
    std::fs::write(f.root().join("grpc/shop.bin"), descriptor_set()).unwrap();
    f.write(
        "grpc/r.rman.yaml",
        &format!("{HEAD}method: shop.v1.Catalog/Chat\nmessages: []\nschema:\n  descriptor_set: shop.bin\n"),
    );
    let resolved = f.resolve("r").unwrap();
    let source = SchemaSource::of(&resolved.schema);
    let schema = load_schema(&source, &nowhere(), &CachePolicy::default())
        .await
        .unwrap();
    assert_eq!(schema.origin, SchemaOrigin::DescriptorSet);
    assert!(schema.method("shop.v1.Catalog/Chat").is_ok());

    f.write("grpc/bad.bin", "not a descriptor set");
    let bad = SchemaSource::DescriptorSet {
        path: f.root().join("grpc/bad.bin"),
        shown: "bad.bin".into(),
    };
    let err = load_schema(&bad, &nowhere(), &CachePolicy::default())
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("grpc: invalid descriptor set bad.bin"),
        "{err}"
    );
}
