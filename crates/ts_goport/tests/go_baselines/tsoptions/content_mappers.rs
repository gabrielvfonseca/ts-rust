//! Rust port of `internal/tsoptions/contentmappers_test.go` (tsgo#4712).
//!
//! PORT: Go runs the tests in parallel (`t.Parallel`); here they run in
//! order. The Go test is in package `tsoptions`; the Rust items it reads
//! (`resolve_content_mapper_manifest`, the `ParsedCommandLine` fields) are
//! `pub`. Go compares mapper pointers; here `Rc::ptr_eq`.

use std::collections::BTreeMap;

use ts_goport::contentmapper::{Definition, Mapper};
use ts_goport::frontend::prelude::*;
use ts_goport::frontend::tsoptions;

use super::tsoptionstest::{file_map, vfs_from_map};

/// Go `&contentmapper.Mapper{Definition: contentmapper.Definition{Package: package, Extensions: extensions}}`.
fn mapper(package: &str, extensions: &[&str]) -> Rc<Mapper> {
    Rc::new(Mapper {
        definition: Definition {
            package: package.to_string(),
            extensions: extensions.iter().map(|ext| (*ext).to_string()).collect(),
            ..Default::default()
        },
        ..Default::default()
    })
}

/// Go `&ParsedCommandLine{ParsedConfig: &ParsedOptions{ContentMappers: mappers}, comparePathsOptions: ...}`.
fn command_line_with_mappers(
    mappers: Vec<Rc<Mapper>>,
    compare_paths_options: ComparePathsOptions,
) -> ParsedCommandLine {
    ParsedCommandLine {
        parsed_config: ParsedOptions {
            content_mappers: mappers,
            ..Default::default()
        },
        compare_paths_options,
        ..Default::default()
    }
}

/// Go `commandLine.GetContentMapperForFileName(fileName) == want`.
fn is_mapper(command_line: &ParsedCommandLine, file_name: &str, want: &Rc<Mapper>) -> bool {
    command_line
        .get_content_mapper_for_file_name(file_name)
        .is_some_and(|got| Rc::ptr_eq(&got, want))
}

// Go: contentmappers_test.go:20 TestGetContentMapperForFileNameUsesLongestExtension
#[test]
fn get_content_mapper_for_file_name_uses_longest_extension() {
    let z_mapper = mapper("z", &[".z"]);
    let yz_mapper = mapper("yz", &[".y.z"]);
    let command_line = command_line_with_mappers(
        vec![z_mapper.clone(), yz_mapper.clone()],
        ComparePathsOptions::default(),
    );

    assert!(is_mapper(&command_line, "/src/Component.y.z", &yz_mapper));
    assert!(is_mapper(&command_line, "/src/Component.z", &z_mapper));
}

// Go: contentmappers_test.go:30 TestGetContentMapperForFileNameUsesHostCaseSensitivity
#[test]
fn get_content_mapper_for_file_name_uses_host_case_sensitivity() {
    let mapper = mapper("", &[".vue"]);
    let insensitive = command_line_with_mappers(
        vec![mapper.clone()],
        ComparePathsOptions {
            use_case_sensitive_file_names: false,
            ..Default::default()
        },
    );
    let sensitive = command_line_with_mappers(
        vec![mapper.clone()],
        ComparePathsOptions {
            use_case_sensitive_file_names: true,
            ..Default::default()
        },
    );

    assert!(is_mapper(&insensitive, "/src/Component.VUE", &mapper));
    assert!(
        sensitive
            .get_content_mapper_for_file_name("/src/Component.VUE")
            .is_none()
    );
}

// Go: contentmappers_test.go:46 TestGetOutputFileNamesExcludesMapperOwnedOutputs
#[test]
fn get_output_file_names_excludes_mapper_owned_outputs() {
    let mapper = mapper("", &[".vue"]);
    let mut command_line = new_parsed_command_line(
        Rc::new(CompilerOptions {
            out_dir: "/dist".to_string(),
            declaration: Tristate::True,
            declaration_map: Tristate::True,
            source_map: Tristate::True,
            ..Default::default()
        }),
        vec!["/src/Component.vue".to_string()],
        None,
        ComparePathsOptions {
            current_directory: "/".to_string(),
            use_case_sensitive_file_names: true,
        },
    );
    command_line.parsed_config.content_mappers = vec![mapper];

    assert_eq!(
        command_line.get_output_file_names().collect::<Vec<_>>(),
        vec!["/dist/Component.d.vue.ts".to_string()]
    );
}

/// Go `map[string]string{...}` of output extensions.
fn output_extensions(entries: &[(&str, &str)]) -> Option<BTreeMap<String, String>> {
    Some(
        entries
            .iter()
            .map(|(source, target)| ((*source).to_string(), (*target).to_string()))
            .collect(),
    )
}

/// Go `&contentmapper.Mapper{Extensions: extensions, OutputExtensions: outputExtensions}`.
fn mapper_with_output_extensions(extensions: &[&str], outputs: &[(&str, &str)]) -> Rc<Mapper> {
    Rc::new(Mapper {
        definition: Definition {
            extensions: extensions.iter().map(|ext| (*ext).to_string()).collect(),
            output_extensions: output_extensions(outputs),
            ..Default::default()
        },
        ..Default::default()
    })
}

// Go: contentmappers_test.go:62 TestContentMapperExtensionRewritesPreserveLongestExtension (ts#64581)
#[test]
fn content_mapper_extension_rewrites_preserve_longest_extension() {
    let mapper = mapper_with_output_extensions(
        &[".z", ".y.z", ".identity.y.z", ".unmapped.y.z"],
        &[
            (".z", ".js"),
            (".y.z", ".mjs"),
            (".identity.y.z", ".identity.y.z"),
        ],
    );
    let mut command_line = new_parsed_command_line(
        Rc::new(CompilerOptions {
            declaration: Tristate::True,
            out_dir: "/dist".to_string(),
            ..Default::default()
        }),
        vec![
            "/src/Widget.y.z".to_string(),
            "/src/Widget.identity.y.z".to_string(),
            "/src/Widget.unmapped.y.z".to_string(),
        ],
        None,
        ComparePathsOptions {
            current_directory: "/".to_string(),
            use_case_sensitive_file_names: true,
        },
    );
    command_line.parsed_config.content_mappers = vec![mapper];
    let rewrites = command_line.content_mapper_extension_rewrites();
    // Go `slices.IsSortedFunc` with `len(b.Source) - len(a.Source)`.
    assert!(
        rewrites
            .windows(2)
            .all(|pair| pair[0].source.len() >= pair[1].source.len())
    );
    for file in ["./Widget.identity.y.z", "./Widget.unmapped.y.z"] {
        assert_eq!(
            rewrite_module_specifier_extension(file, rewrites, true),
            None,
            "{file}"
        );
    }
    assert_eq!(
        command_line.get_output_file_names().collect::<Vec<_>>(),
        vec![
            "/dist/Widget.d.mts".to_string(),
            "/dist/Widget.d.identity.y.z.ts".to_string(),
            "/dist/Widget.d.unmapped.y.z.ts".to_string(),
        ]
    );
}

// Go: contentmappers_test.go:94 TestContentMapperExtensionRewritesAreCached (ts#64581)
// PORT: Go also counts allocations (`testing.AllocsPerRun`) and runs the
// copy's reads in parallel subtests. The port has no allocation counter, and
// its command line is single-threaded (`OnceCell`); the same slice pointer
// on each read is the cache test.
#[test]
fn content_mapper_extension_rewrites_are_cached() {
    let mapper =
        mapper_with_output_extensions(&[".astro", ".y.z"], &[(".astro", ".js"), (".y.z", ".mjs")]);
    let mut command_line = new_parsed_command_line(
        Rc::new(CompilerOptions::default()),
        Vec::new(),
        None,
        ComparePathsOptions {
            current_directory: "/".to_string(),
            use_case_sensitive_file_names: true,
        },
    );
    command_line.parsed_config.content_mappers = vec![mapper];
    let rewrites = command_line.content_mapper_extension_rewrites().to_vec();
    let first = command_line.content_mapper_extension_rewrites().as_ptr();
    assert!(!rewrites.is_empty());
    for _ in 0..10 {
        assert_eq!(
            command_line.content_mapper_extension_rewrites().as_ptr(),
            first
        );
    }
    let with_file_names = command_line.with_file_names(vec!["/src/Card.astro".to_string()]);
    for _ in 0..10 {
        assert_eq!(
            command_line.content_mapper_extension_rewrites().as_ptr(),
            first
        );
        assert_eq!(
            with_file_names.content_mapper_extension_rewrites(),
            rewrites.as_slice()
        );
    }
}

// Go: contentmappers_test.go:121 TestResolveContentMapperManifestRejectsMalformedOutputExtensions (ts#64581)
#[test]
fn resolve_content_mapper_manifest_rejects_malformed_output_extensions() {
    for value in [
        r#""invalid""#,
        "null",
        "[]",
        r#"{".vue": 1}"#,
        r#"{".vue": null}"#,
        r#"{".vue/foo": ".js"}"#,
        r#"{".vue\\foo": ".js"}"#,
        r#"{".vue": ".js/foo"}"#,
        r#"{".vue": ".js\\foo"}"#,
    ] {
        let package_json = format!(
            "{{\n\t\t\t\t\t\"name\": \"mapper\",\n\t\t\t\t\t\"typescript\": {{ \"contentMapper\": {{ \"exec\": [\"run\"], \"outputExtensions\": {value} }} }}\n\t\t\t\t}}"
        );
        let host = ResolveContentMapperHost {
            fs: vfs_from_map(
                &file_map(&[(
                    "/home/project/node_modules/mapper/package.json",
                    &package_json,
                )]),
                true, /*useCaseSensitiveFileNames*/
            ),
        };
        let (_, _, diagnostic) = tsoptions::resolve_content_mapper_manifest(
            &host,
            "/home/project/tsconfig.json",
            "mapper",
        );
        assert!(
            diagnostic.is_some(),
            "{value}: expected malformed outputExtensions to be rejected"
        );
    }
}

// Go: contentmappers_test.go:16 resolveContentMapperHost
struct ResolveContentMapperHost {
    fs: Rc<dyn Fs>,
}

impl ParseConfigHost for ResolveContentMapperHost {
    // Go: contentmappers_test.go:64 (resolveContentMapperHost).FS
    fn fs(&self) -> Rc<dyn Fs> {
        self.fs.clone()
    }

    // Go: contentmappers_test.go:65 (resolveContentMapperHost).GetCurrentDirectory
    fn get_current_directory(&self) -> String {
        "/home/project".to_string()
    }
}

// Go: contentmappers_test.go:138 TestResolveContentMapperManifest (ts#64581)
#[test]
fn resolve_content_mapper_manifest() {
    let host = ResolveContentMapperHost {
        fs: vfs_from_map(
            &file_map(&[
                (
                    "/home/project/node_modules/vue-ts-mapper/package.json",
                    "{\n\t\t\t\"name\": \"vue-ts-mapper\",\n\t\t\t\"version\": \"1.2.3\",\n\t\t\t\"typescript\": { \"contentMapper\": { \"exec\": [\"node\", \"./dist/mapper.js\"], \"compilerOptions\": [\"target\", \"jsx\"], \"outputExtensions\": { \".vue\": \".js\" } } }\n\t\t}",
                ),
                (
                    "/home/node_modules/@scope/noversion/package.json",
                    "{\n\t\t\t\"name\": \"@scope/noversion\",\n\t\t\t\"typescript\": { \"contentMapper\": { \"exec\": [\"run\"] } }\n\t\t}",
                ),
                (
                    "/home/project/node_modules/no-name/package.json",
                    "{\n\t\t\t\"version\": \"1.0.0\"\n\t\t}",
                ),
                (
                    "/home/project/node_modules/no-manifest/package.json",
                    "{\n\t\t\t\"name\": \"no-manifest\"\n\t\t}",
                ),
                (
                    "/home/project/node_modules/no-exec/package.json",
                    "{\n\t\t\t\"name\": \"no-exec\",\n\t\t\t\"typescript\": { \"contentMapper\": {} }\n\t\t}",
                ),
                (
                    "/home/project/node_modules/bad-exec/package.json",
                    "{\n\t\t\t\"name\": \"bad-exec\",\n\t\t\t\"typescript\": { \"contentMapper\": { \"exec\": \"node ./mapper.js\" } }\n\t\t}",
                ),
                (
                    "/home/project/node_modules/bad-output-extension/package.json",
                    "{\n\t\t\t\"name\": \"bad-output-extension\",\n\t\t\t\"typescript\": { \"contentMapper\": { \"exec\": [\"run\"], \"outputExtensions\": { \".vue\": \"js\" } } }\n\t\t}",
                ),
            ]),
            true, /*useCaseSensitiveFileNames*/
        ),
    };
    let code = |message: &'static Message| message.code() as i32;

    // Name, version, and the verbatim exec argv are preserved.
    let (manifest, package_directory, diagnostic) = tsoptions::resolve_content_mapper_manifest(
        &host,
        "/home/project/tsconfig.json",
        "vue-ts-mapper",
    );
    assert!(diagnostic.is_none());
    assert_eq!(manifest.name, "vue-ts-mapper");
    assert_eq!(manifest.version, "1.2.3");
    assert_eq!(
        package_directory,
        "/home/project/node_modules/vue-ts-mapper"
    );
    assert_eq!(
        manifest.exec,
        vec!["node".to_string(), "./dist/mapper.js".to_string()]
    );
    assert_eq!(
        manifest.compiler_options,
        vec!["target".to_string(), "jsx".to_string()]
    );
    assert_eq!(
        manifest.default_output_extensions,
        output_extensions(&[(".vue", ".js")])
    );

    // Resolution walks up node_modules; a package with no version resolves to a name and empty version.
    let (manifest, _, diagnostic) = tsoptions::resolve_content_mapper_manifest(
        &host,
        "/home/project/src/tsconfig.json",
        "@scope/noversion",
    );
    assert!(diagnostic.is_none());
    assert_eq!(manifest.name, "@scope/noversion");
    assert_eq!(manifest.version, "");

    // A package that is not installed reports a resolution diagnostic.
    let (_, _, diagnostic) = tsoptions::resolve_content_mapper_manifest(
        &host,
        "/home/project/tsconfig.json",
        "missing-mapper",
    );
    let diagnostic = diagnostic.expect("expected a diagnostic for missing-mapper");
    assert_eq!(
        diagnostic.code,
        code(diag::The_content_mapper_package_0_could_not_be_resolved)
    );

    // A package whose package.json has no name reports a diagnostic.
    let (_, package_directory, diagnostic) =
        tsoptions::resolve_content_mapper_manifest(&host, "/home/project/tsconfig.json", "no-name");
    let diagnostic = diagnostic.expect("expected a diagnostic for no-name");
    assert_eq!(package_directory, "/home/project/node_modules/no-name");
    assert_eq!(
        diagnostic.code,
        code(diag::The_package_json_of_the_content_mapper_package_0_does_not_specify_a_name)
    );

    // A package that does not declare a "typescript.contentMapper" object reports a diagnostic.
    let (_, _, diagnostic) = tsoptions::resolve_content_mapper_manifest(
        &host,
        "/home/project/tsconfig.json",
        "no-manifest",
    );
    let diagnostic = diagnostic.expect("expected a diagnostic for no-manifest");
    assert_eq!(
        diagnostic.code,
        code(
            diag::The_package_json_of_the_content_mapper_package_0_does_not_declare_a_typescript_contentMapper_object
        )
    );

    // A "typescript.contentMapper" with no "exec", or an "exec" of the wrong type, reports a diagnostic.
    for pkg in ["no-exec", "bad-exec"] {
        let (_, _, diagnostic) =
            tsoptions::resolve_content_mapper_manifest(&host, "/home/project/tsconfig.json", pkg);
        let diagnostic = diagnostic.unwrap_or_else(|| panic!("expected a diagnostic for {pkg}"));
        assert_eq!(
            diagnostic.code,
            code(
                diag::The_typescript_contentMapper_exec_of_the_content_mapper_package_0_must_be_a_non_empty_array_of_strings
            )
        );
    }

    let (_, _, diagnostic) = tsoptions::resolve_content_mapper_manifest(
        &host,
        "/home/project/tsconfig.json",
        "bad-output-extension",
    );
    assert_eq!(
        diagnostic.map(|diagnostic| diagnostic.code),
        Some(code(
            diag::The_typescript_contentMapper_outputExtensions_of_the_content_mapper_package_0_contains_an_invalid_mapping_from_1_to_2_Extensions_must_be_non_empty_and_begin_with_a
        ))
    );
}
