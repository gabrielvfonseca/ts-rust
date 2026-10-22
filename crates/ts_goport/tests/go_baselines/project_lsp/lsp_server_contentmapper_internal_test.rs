//! Port of Go `internal/lsp/server_contentmapper_internal_test.go`
//! (tsgo#4712, ts#64581).
//!
//! PORT: Go tests package `lsp` from inside. The functions are `pub` in the
//! port. Go `&Server{stderr: &output}` is `lsp::new_server` with a stderr
//! that the test reads; the server never runs.

use std::io::Write;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use indexmap::IndexMap;
use ts_goport::frontend::bundled;
use ts_goport::frontend::json_ext::LspAny;
use ts_goport::frontend::tspath;
use ts_goport::gostd::{GoError, errors};
use ts_goport::lsp::{self, lsproto};

use crate::support::vfstest::{MapFile, MapFs};

/// Go `strings.Builder` as the server's stderr.
#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);

impl Output {
    fn string(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
    }
}

impl Write for Output {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A reader at end of input (the server never runs).
struct NoInput;

impl lsp::Reader for NoInput {
    fn read(&mut self) -> (Option<lsproto::Message>, Option<GoError>) {
        (None, Some(errors::EOF.clone()))
    }
}

/// A writer that drops every message (the server never runs).
struct NoOutput;

impl lsp::Writer for NoOutput {
    fn write(&mut self, _msg: &lsproto::Message) -> Result<(), GoError> {
        Ok(())
    }
}

/// Go `lsproto.ContentMapperManifest{Name: name, Exec: exec}` with the
/// other fields nil.
fn manifest(name: &str, exec: &[&str]) -> lsproto::ContentMapperManifest {
    lsproto::ContentMapperManifest {
        name: name.to_string(),
        version: None,
        exec: exec.iter().map(|arg| (*arg).to_string()).collect(),
        cwd: None,
        compiler_options: None,
        dynamic_config: None,
        output_extensions: None,
    }
}

/// `parseContentMapperContributions` must fail with an error that holds
/// `text` (Go `assert.ErrorContains`).
fn assert_error_contains(
    result: Result<ts_goport::project::ContentMapperContributions, GoError>,
    text: &str,
) {
    match result {
        Ok(_) => panic!("expected an error that contains {text:?}"),
        Err(err) => assert!(err.error().contains(text), "{}", err.error()),
    }
}

child_test! {
    // Go: server_contentmapper_internal_test.go:13 TestContentMapperLoggerRequiresTrace
    fn content_mapper_logger_requires_trace() {
        let output = Output::default();
        let server = lsp::new_server(lsp::ServerOptions {
            in_: Box::new(NoInput),
            out: Box::new(NoOutput),
            err: Box::new(output.clone()),
            cwd: "/".to_string(),
            fs: bundled::wrap_fs(MapFs::from_map(Vec::<(String, MapFile)>::new(), false).fs()),
            default_library_path: bundled::lib_path(),
            typings_location: String::new(),
            parse_cache: None,
            npm_install: None,
            spawn: None,
            progress_delay: Duration::ZERO,
            set_parent_process_id: None,
        });
        let logger = server.content_mapper_logger();
        logger("hidden");
        assert!(output.string().is_empty());
        server.logger.set_verbosity(lsproto::LogVerbosity::TRACE);
        logger("visible");
        assert!(output.string().contains("visible"));
    }
}

child_test! {
    // Go: server_contentmapper_internal_test.go:26 TestParseContentMapperContributions (ts#64581)
    fn parse_content_mapper_contributions() {
        let version = "2.3.4";
        let cwd = "/workspace/mapper";
        let output_extensions: IndexMap<String, String> =
            IndexMap::from([(".vue".to_string(), ".js".to_string())]);
        let mut options: IndexMap<String, LspAny> = IndexMap::new();
        options.insert("mode".to_string(), LspAny::String("embedded".to_string()));
        // PORT: Go keeps a pointer to `outputExtensions` in the contribution;
        // here the test changes the contribution's copy after the call.
        let mut values = [
            Some(lsproto::ContentMapperContribution {
                contributor_id: "publisher.extension".to_string(),
                extensions: vec![".vue".to_string()],
                inferred_project_contribution: Some(lsproto::InferredProjectContentMapperContribution {
                    options: Some(options),
                    manifest: Some(lsproto::ContentMapperManifest {
                        name: "Vue mapper".to_string(),
                        version: Some(version.to_string()),
                        exec: vec!["node".to_string(), "mapper.js".to_string()],
                        cwd: Some(cwd.to_string()),
                        compiler_options: Some(vec!["strict".to_string()]),
                        dynamic_config: None,
                        output_extensions: Some(output_extensions),
                    }),
                }),
            }),
            Some(lsproto::ContentMapperContribution {
                contributor_id: "publisher.extension".to_string(),
                extensions: vec![".svelte".to_string()],
                inferred_project_contribution: None,
            }),
        ];
        let mut contributions = lsp::parse_content_mapper_contributions(&values)
            .unwrap_or_else(|err| panic!("parseContentMapperContributions: {}", err.error()));
        assert_eq!(contributions.mappers.len(), 1);
        assert_eq!(contributions.extensions, vec![".vue".to_string()]);
        let mapper = Rc::get_mut(&mut contributions.mappers[0]).expect("one reference to the mapper");
        assert_eq!(mapper.identity(), "publisher.extension[0] (Vue mapper@2.3.4)");
        assert_eq!(mapper.package_directory, cwd);
        let want = Some([(".vue".to_string(), ".js".to_string())].into_iter().collect());
        assert_eq!(mapper.manifest.default_output_extensions, want);
        assert_eq!(mapper.definition.output_extensions, want);
        values[0]
            .as_mut()
            .and_then(|value| value.inferred_project_contribution.as_mut())
            .and_then(|contribution| contribution.manifest.as_mut())
            .and_then(|manifest| manifest.output_extensions.as_mut())
            .expect("the contribution's outputExtensions")
            .insert(".vue".to_string(), ".mjs".to_string());
        assert_eq!(output_extension(mapper, ".vue"), ".js");
        mapper
            .manifest
            .default_output_extensions
            .as_mut()
            .expect("DefaultOutputExtensions")
            .insert(".vue".to_string(), ".cjs".to_string());
        assert_eq!(output_extension(mapper, ".vue"), ".js");
        assert_eq!(
            String::from_utf8_lossy(&mapper.definition.options.0),
            r#"{"mode":"embedded"}"#
        );
    }
}

child_test! {
    // Go: server_contentmapper_internal_test.go:69 TestParseContentMapperContributionsRejectsConflictingInlineMappers
    fn parse_content_mapper_contributions_rejects_conflicting_inline_mappers() {
        let inferred_project_contribution = |name: &str| lsproto::InferredProjectContentMapperContribution {
            options: None,
            manifest: Some(manifest(name, &[name])),
        };
        let result = lsp::parse_content_mapper_contributions(&[
            Some(lsproto::ContentMapperContribution {
                contributor_id: "first".to_string(),
                extensions: vec![".vue".to_string()],
                inferred_project_contribution: Some(inferred_project_contribution("first")),
            }),
            Some(lsproto::ContentMapperContribution {
                contributor_id: "second".to_string(),
                extensions: vec![".vue".to_string()],
                inferred_project_contribution: Some(inferred_project_contribution("second")),
            }),
        ]);
        match result {
            Ok(_) => panic!("expected an error"),
            Err(err) => assert!(
                err.error().contains(r#"both claim extension ".vue""#),
                "{}",
                err.error()
            ),
        }
    }
}

child_test! {
    // Go: server_contentmapper_internal_test.go:81 TestParseContentMapperContributionsUsesCaseInsensitiveExtensions (ts#63936)
    fn parse_content_mapper_contributions_uses_case_insensitive_extensions() {
        let inferred_project_contribution = |name: &str| lsproto::InferredProjectContentMapperContribution {
            options: None,
            manifest: Some(manifest(name, &[name])),
        };
        let result = lsp::parse_content_mapper_contributions(&[
            Some(lsproto::ContentMapperContribution {
                contributor_id: "first".to_string(),
                extensions: vec![".vue".to_string()],
                inferred_project_contribution: Some(inferred_project_contribution("first")),
            }),
            Some(lsproto::ContentMapperContribution {
                contributor_id: "second".to_string(),
                extensions: vec![".VUE".to_string()],
                inferred_project_contribution: Some(inferred_project_contribution("second")),
            }),
        ]);
        match result {
            Ok(_) => panic!("expected an error"),
            Err(err) => assert!(
                err.error().contains(r#"both claim extension ".VUE""#),
                "{}",
                err.error()
            ),
        }

        let result = lsp::parse_content_mapper_contributions(&[Some(lsproto::ContentMapperContribution {
            contributor_id: "built-in".to_string(),
            extensions: vec![".TS".to_string()],
            inferred_project_contribution: None,
        })]);
        match result {
            Ok(_) => panic!("expected an error"),
            Err(err) => assert!(
                err.error().contains(r#"invalid extension ".TS""#),
                "{}",
                err.error()
            ),
        }
    }
}

child_test! {
    // Go: server_contentmapper_internal_test.go:99 TestParseContentMapperContributionsDefaultsOptionsToObject
    fn parse_content_mapper_contributions_defaults_options_to_object() {
        let contributions = lsp::parse_content_mapper_contributions(&[Some(lsproto::ContentMapperContribution {
            contributor_id: "publisher.extension".to_string(),
            extensions: vec![".vue".to_string()],
            inferred_project_contribution: Some(lsproto::InferredProjectContentMapperContribution {
                options: None,
                manifest: Some(manifest("mapper", &["mapper"])),
            }),
        })])
        .unwrap_or_else(|err| panic!("parseContentMapperContributions: {}", err.error()));
        assert_eq!(
            String::from_utf8_lossy(&contributions.mappers[0].definition.options.0),
            "{}"
        );
    }
}

/// Go `mapper.Definition.OutputExtensions[source]` (a nil map or a missing
/// key reads "").
fn output_extension<'a>(mapper: &'a ts_goport::contentmapper::Mapper, source: &str) -> &'a str {
    mapper
        .definition
        .output_extensions
        .as_ref()
        .and_then(|extensions| extensions.get(source))
        .map_or("", String::as_str)
}

/// Go `&lsproto.ContentMapperContribution{ContributorId: id, Extensions: []string{extension}}`.
fn contribution(id: &str, extension: &str) -> Option<lsproto::ContentMapperContribution> {
    Some(lsproto::ContentMapperContribution {
        contributor_id: id.to_string(),
        extensions: vec![extension.to_string()],
        inferred_project_contribution: None,
    })
}

child_test! {
    // Go: server_contentmapper_internal_test.go:112 TestParseContentMapperContributionsCompoundExtensions (ts#64581)
    fn parse_content_mapper_contributions_compound_extensions() {
        for extension in [".y.z", ".component.tsx.vue"] {
            let mut value = contribution("mapper", extension);
            if let Some(value) = value.as_mut() {
                value.inferred_project_contribution =
                    Some(lsproto::InferredProjectContentMapperContribution {
                        options: None,
                        manifest: Some(manifest("mapper", &["mapper"])),
                    });
            }
            let contributions = lsp::parse_content_mapper_contributions(&[value])
                .unwrap_or_else(|err| panic!("parseContentMapperContributions: {}", err.error()));
            assert_eq!(contributions.extensions, vec![extension.to_string()]);
        }
        for native_extension in tspath::ALL_SUPPORTED_EXTENSIONS_WITH_JSON.iter().flat_map(|group| group.iter()) {
            for extension in [
                format!(".component{native_extension}"),
                format!(".component{}", native_extension.to_uppercase()),
            ] {
                assert_error_contains(
                    lsp::parse_content_mapper_contributions(&[contribution("mapper", &extension)]),
                    "invalid extension",
                );
            }
        }
    }
}

child_test! {
    // Go: server_contentmapper_internal_test.go:135 TestParseContentMapperContributionsRejectsInvalidOutputExtensions (ts#64581)
    fn parse_content_mapper_contributions_rejects_invalid_output_extensions() {
        for compiler_options in [None, Some(Vec::new()), Some(vec!["strict".to_string()])] {
            let output_extensions = IndexMap::from([(".vue".to_string(), "js".to_string())]);
            let result = lsp::parse_content_mapper_contributions(&[Some(lsproto::ContentMapperContribution {
                contributor_id: "publisher.extension".to_string(),
                extensions: vec![".vue".to_string()],
                inferred_project_contribution: Some(lsproto::InferredProjectContentMapperContribution {
                    options: None,
                    manifest: Some(lsproto::ContentMapperManifest {
                        compiler_options,
                        output_extensions: Some(output_extensions),
                        ..manifest("mapper", &["mapper"])
                    }),
                }),
            })]);
            assert_error_contains(result, "invalid output extension mapping");
        }
    }
}

child_test! {
    // Go: server_contentmapper_internal_test.go:153 TestParseContentMapperContributionsRejectsExtensionPaths (ts#64581)
    fn parse_content_mapper_contributions_rejects_extension_paths() {
        for extension in [".vue/foo", r".vue\foo"] {
            assert_error_contains(
                lsp::parse_content_mapper_contributions(&[contribution("mapper", extension)]),
                "invalid extension",
            );
            for output_extensions in [
                IndexMap::from([(extension.to_string(), ".js".to_string())]),
                IndexMap::from([(".vue".to_string(), extension.to_string())]),
            ] {
                let mut value = contribution("mapper", ".vue");
                if let Some(value) = value.as_mut() {
                    value.inferred_project_contribution =
                        Some(lsproto::InferredProjectContentMapperContribution {
                            options: None,
                            manifest: Some(lsproto::ContentMapperManifest {
                                output_extensions: Some(output_extensions),
                                ..manifest("mapper", &["mapper"])
                            }),
                        });
                }
                assert_error_contains(
                    lsp::parse_content_mapper_contributions(&[value]),
                    "invalid output extension mapping",
                );
            }
        }
    }
}
