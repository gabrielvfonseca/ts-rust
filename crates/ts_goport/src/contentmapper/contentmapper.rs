//! Port of Go `internal/contentmapper/contentmapper.go` (tsgo#4712).
//!
//! Package contentmapper defines the types describing an external content mapper: a plugin that
//! transforms otherwise unsupported file content (e.g. .vue) into virtual TypeScript during program construction.
//!
//! A mapper is declared in tsconfig (Definition), its implementation is described by fields in its npm
//! package's package.json (Manifest), and the two are combined once the package is resolved (Mapper).
//! Resolution itself lives in the tsoptions package (it needs node module resolution).
//!
//! The package also drives the configured content mappers at build time (Host): it spawns each mapper's
//! package as a child process and talks to it over a JSON-RPC connection (reusing internal/ipc), turning
//! content-mapped source files into virtual TypeScript. Processes are consolidated by mapper identity, so many projects
//! that use the same mapper version share a single process.

use crate::contentmapper::prelude::*;

use crate::frontend::json_ext::{
    marshal_field, unmarshal_struct_fields, write_object_end, write_object_start,
};
use crate::options_json::{CompilerOptionsJSON, marshal_field_omitempty};
// PORT: the package's `Result` (host.go) comes with the glob import; this
// file uses only `std::result::Result`, so the explicit import picks it.
use std::collections::BTreeMap;
use std::result::Result;
use std::sync::LazyLock;

// Go: contentmapper/contentmapper.go:29 ErrProjectUnavailable
pub static ERR_PROJECT_UNAVAILABLE: LazyLock<GoError> =
    LazyLock::new(|| errors::new("content mapper project is unavailable"));

// Go: contentmapper/contentmapper.go:33 Definition
// Definition is a content mapper as declared in a tsconfig's "contentMappers": the npm package that
// implements the mapper and the otherwise unsupported file extensions it registers.
// PORT: a Go `map[string]string` is a `BTreeMap`, so its keys iterate in the
// order of Go's `slices.Sorted(maps.Keys(...))`. A nil Go map is `None`: an
// empty `outputExtensions` object is not the same as no object (ts#64581).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Definition {
    pub package: String,
    pub extensions: Vec<String>,
    // OutputExtensions contains the resolved mapping after applying manifest defaults and project overrides.
    pub output_extensions: Option<BTreeMap<String, String>>,
    pub options: JsonValue,
}

// Go JSON v2 struct arshaler for `Definition` (tags `package`, `extensions`,
// `outputExtensions,omitzero`, `options,omitempty`).
impl MarshalerTo for Definition {
    fn marshal_json_to(&self, enc: &mut String) -> Result<(), JsonError> {
        let first = &mut true;
        write_object_start(enc);
        self.marshal_fields(enc, first)?;
        write_object_end(enc);
        Ok(())
    }
}

impl Definition {
    /// The members of `Definition`, for the structs that embed it (Go
    /// inlines an embedded struct's fields).
    fn marshal_fields(&self, enc: &mut String, first: &mut bool) -> Result<(), JsonError> {
        marshal_field(enc, first, "package", &self.package)?;
        marshal_field(enc, first, "extensions", &self.extensions)?;
        // Go `omitzero`: only a nil map is zero.
        if let Some(output_extensions) = &self.output_extensions {
            if !*first {
                enc.push(',');
            }
            *first = false;
            "outputExtensions".marshal_json_to(enc)?;
            enc.push(':');
            marshal_string_map(enc, output_extensions)?;
        }
        marshal_field_omitempty(enc, first, "options", &self.options)
    }

    /// Decodes one member of `Definition`; false for an unknown name.
    fn unmarshal_field(
        &mut self,
        name: &str,
        dec: &mut JsonDecoder<'_>,
    ) -> Result<bool, JsonError> {
        match name {
            "package" => json_unmarshal_decode(dec, &mut self.package)?,
            "extensions" => json_unmarshal_decode(dec, &mut self.extensions)?,
            "outputExtensions" => {
                // PORT: decoded with the Go v2 map arshaler (`null` sets a
                // nil map; an object merges into the existing map).
                let mut map: Option<FxHashMap<String, String>> = self
                    .output_extensions
                    .take()
                    .map(|map| map.into_iter().collect());
                let result = json_unmarshal_decode(dec, &mut map);
                self.output_extensions = map.map(|map| map.into_iter().collect());
                result?;
            }
            "options" => json_unmarshal_decode(dec, &mut self.options)?,
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// Go v2 marshal of a `map[string]string`, keys in sorted order (Go
/// `json.Deterministic(true)`; without it Go's order is unspecified).
fn marshal_string_map(enc: &mut String, map: &BTreeMap<String, String>) -> Result<(), JsonError> {
    enc.push('{');
    for (i, (key, value)) in map.iter().enumerate() {
        if i > 0 {
            enc.push(',');
        }
        key.marshal_json_to(enc)?;
        enc.push(':');
        value.marshal_json_to(enc)?;
    }
    enc.push('}');
    Ok(())
}

impl UnmarshalerFrom for Definition {
    fn unmarshal_json_from(&mut self, dec: &mut JsonDecoder<'_>) -> Result<(), JsonError> {
        let is_object = unmarshal_struct_fields(dec, "contentmapper.Definition", |name, dec| {
            self.unmarshal_field(name, dec)
        })?;
        if !is_object {
            *self = Definition::default();
        }
        Ok(())
    }
}

// Go: contentmapper/contentmapper.go:44 Manifest
// Manifest is the content-mapper information read from a package's package.json: its name and version
// (which form the mapper's identity), the argv used to run it, and the compiler options it declares it
// depends on.
// PORT: `default_output_extensions` is a Go `map[string]string`; see
// `Definition`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub exec: Vec<String>,
    pub compiler_options: Vec<String>,
    pub dynamic_config: bool,
    pub default_output_extensions: Option<BTreeMap<String, String>>,
}

// Go: contentmapper/contentmapper.go:55 Mapper
// Mapper is a resolved content mapper: its tsconfig Definition combined with the Manifest resolved from
// the package's package.json, plus the package directory used as the mapper's working directory.
// PORT: Go embeds `Definition` and `Manifest`; here they are the fields
// `definition` and `manifest` (Go `m.Name` is `m.manifest.name`, Go
// `m.Package` is `m.definition.package`). Go `*Mapper` is `Rc<Mapper>`, and
// Go compares and keys mappers by pointer (`Rc::as_ptr`). `==` is Go
// `Mapper.Equals` (ts#64457).
#[derive(Clone, Debug, Default)]
pub struct Mapper {
    pub definition: Definition,
    // json:"-"
    pub manifest: Manifest,
    // PackageDirectory is the real path directory returned by package resolution for package-based mappers.
    // json:"-"
    pub package_directory: String,
    // ContributionID is provided by an LSP client extension for inferred project content mappers.
    // json:"-"
    pub contribution_id: String,
}

impl PartialEq for Mapper {
    fn eq(&self, other: &Self) -> bool {
        self.equals(other)
    }
}

// Go JSON v2: the embedded `Definition` is inlined; the other fields are
// `json:"-"`.
impl MarshalerTo for Mapper {
    fn marshal_json_to(&self, enc: &mut String) -> Result<(), JsonError> {
        let first = &mut true;
        write_object_start(enc);
        self.definition.marshal_fields(enc, first)?;
        write_object_end(enc);
        Ok(())
    }
}

impl UnmarshalerFrom for Mapper {
    fn unmarshal_json_from(&mut self, dec: &mut JsonDecoder<'_>) -> Result<(), JsonError> {
        let is_object = unmarshal_struct_fields(dec, "contentmapper.Mapper", |name, dec| {
            self.definition.unmarshal_field(name, dec)
        })?;
        if !is_object {
            *self = Mapper::default();
        }
        Ok(())
    }
}

// Go: contentmapper/contentmapper.go:64 supportedVirtualExtensions
static SUPPORTED_VIRTUAL_EXTENSIONS: LazyLock<FxHashSet<&'static str>> = LazyLock::new(|| {
    [
        ".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".json",
    ]
    .into_iter()
    .collect()
});

// Go: contentmapper/contentmapper.go:68 IsSupportedVirtualExtension
#[must_use]
pub fn is_supported_virtual_extension(extension: &str) -> bool {
    SUPPORTED_VIRTUAL_EXTENSIONS.contains(extension)
}

// Go: contentmapper/contentmapper.go:72 IsValidExtension (ts#64581)
/// A dot, at least one more byte, and no path separator.
#[must_use]
pub fn is_valid_extension(extension: &str) -> bool {
    extension.len() > 1 && extension.starts_with('.') && !extension.contains(['/', '\\'])
}

// Go: contentmapper/contentmapper.go:76 HasReservedSourceExtension (ts#64581)
/// Whether `extension` ends with a built-in extension (`.ts`, `.d.ts`,
/// `.json`, ...), in any case.
// PORT: Go compares the bytes of the suffix with `strings.EqualFold`.
#[must_use]
pub fn has_reserved_source_extension(extension: &str) -> bool {
    for extensions in tspath::ALL_SUPPORTED_EXTENSIONS_WITH_JSON {
        for native_extension in *extensions {
            if extension.len() >= native_extension.len()
                && crate::frontend::vfs::vfsmatch::equal_fold(
                    &extension.as_bytes()[extension.len() - native_extension.len()..],
                    native_extension.as_bytes(),
                )
            {
                return true;
            }
        }
    }
    false
}

impl Mapper {
    // Go: contentmapper/contentmapper.go:89 Mapper.DiagnosticName
    // DiagnosticName returns the best available user-facing name, including when manifest resolution failed.
    #[must_use]
    pub fn diagnostic_name(&self) -> String {
        if !self.manifest.name.is_empty() {
            self.manifest.name.clone()
        } else if !self.definition.package.is_empty() {
            self.definition.package.clone()
        } else {
            self.contribution_id.clone()
        }
    }

    // Go: contentmapper/contentmapper.go:102 Mapper.Identity
    // Identity returns the mapper's "name@version" identity, or just the name when it declares no version,
    // or an empty string when the mapper has not been resolved to a name.
    #[must_use]
    pub fn identity(&self) -> String {
        if !self.contribution_id.is_empty() {
            return format!("{} ({})", self.contribution_id, self.manifest_identity());
        }
        self.manifest_identity()
    }

    // Go: contentmapper/contentmapper.go:109 Mapper.manifestIdentity
    fn manifest_identity(&self) -> String {
        if self.manifest.name.is_empty() {
            String::new()
        } else if self.manifest.version.is_empty() {
            self.manifest.name.clone()
        } else {
            format!("{}@{}", self.manifest.name, self.manifest.version)
        }
    }

    // Go: contentmapper/contentmapper.go:121 Mapper.Equals (ts#64457, ts#64581)
    // Equals compares the complete mapper configuration, not just its advertised identity.
    // PORT: Go `m == other` is `ptr::eq`. A nil `*Mapper` has no Rust form.
    // Go also tells a nil slice from an empty one (`Extensions`, `Options`,
    // `Exec`, `CompilerOptions`); the port has no nil slice, so those are equal.
    // A nil map is `None`, so `==` on the output extension maps is Go's nil
    // test and `maps.Equal`.
    #[must_use]
    pub fn equals(&self, other: &Mapper) -> bool {
        if std::ptr::eq(self, other) {
            return true;
        }
        self.definition.package == other.definition.package
            && self.definition.extensions == other.definition.extensions
            && self.definition.output_extensions == other.definition.output_extensions
            && self.definition.options == other.definition.options
            && self.manifest.name == other.manifest.name
            && self.manifest.version == other.manifest.version
            && self.manifest.exec == other.manifest.exec
            && self.manifest.compiler_options == other.manifest.compiler_options
            && self.manifest.dynamic_config == other.manifest.dynamic_config
            && self.manifest.default_output_extensions == other.manifest.default_output_extensions
            && self.package_directory == other.package_directory
            && self.contribution_id == other.contribution_id
    }

    // Go: contentmapper/contentmapper.go:151 Mapper.TransformIdentity (ts#64581)
    // TransformIdentity fingerprints the mapper's identity, declared compiler options, and runtime output
    // contract so both transformed-file caches and incremental emit are invalidated when these change.
    // It is a pure function of the mapper and options and never starts the mapper process.
    // PORT: Go `xxh3.Uint128` is `u128` (`Hi << 64 | Lo`); Go `.Bytes()` is
    // `to_be_bytes()`. Go `*core.CompilerOptions` nil is `None`.
    #[must_use]
    pub fn transform_identity(&self, options: Option<&CompilerOptions>) -> u128 {
        // Go ignores the error: `declared` is then a nil map, which marshals as `null`.
        let options_json = match self.marshal_declared_options(options) {
            Ok(declared) => json_marshal(&declared, &[]).unwrap_or_default(),
            Err(_) => "null".to_string(),
        };
        // Go marshals a non-empty map with `json.Deterministic(true)`.
        let mut output_extensions_json = String::new();
        if let Some(output_extensions) = self
            .definition
            .output_extensions
            .as_ref()
            .filter(|map| !map.is_empty())
        {
            let _ = marshal_string_map(&mut output_extensions_json, output_extensions);
        }
        let identity = self.identity();
        let mut buf: Vec<u8> = Vec::with_capacity(
            identity.len()
                + 3
                + self.definition.options.0.len()
                + options_json.len()
                + output_extensions_json.len(),
        );
        buf.extend_from_slice(identity.as_bytes());
        buf.push(0);
        buf.extend_from_slice(&self.definition.options.0);
        buf.push(0);
        buf.extend_from_slice(options_json.as_bytes());
        if !output_extensions_json.is_empty() {
            buf.push(0);
            buf.extend_from_slice(output_extensions_json.as_bytes());
        }
        xxhash_rust::xxh3::xxh3_128(&buf)
    }

    // Go: contentmapper/contentmapper.go:174 Mapper.MarshalDeclaredOptions
    // MarshalDeclaredOptions marshals just the compiler options this mapper declared it depends on, in the
    // declared order, skipping any that are unset. Marshaling only the declared fields avoids serializing the
    // whole CompilerOptions when a mapper depends on few options (or none).
    // PORT: Go returns `*collections.OrderedMap[string, json.Value]`; here an
    // `IndexMap` in insertion order.
    pub fn marshal_declared_options(
        &self,
        options: Option<&CompilerOptions>,
    ) -> Result<IndexMap<String, JsonValue>, GoError> {
        let mut out: IndexMap<String, JsonValue> =
            IndexMap::with_capacity(self.manifest.compiler_options.len());
        let Some(options) = options else {
            return Ok(out);
        };
        if self.manifest.compiler_options.is_empty() {
            return Ok(out);
        }
        let fields = compiler_option_fields(options)?;
        for name in &self.manifest.compiler_options {
            // PORT: `fields` holds only the options that are set (see
            // `compiler_option_fields`), so a missing name is both Go's
            // unknown field and Go's `field.IsZero()`.
            let Some(raw) = fields.get(name.as_str()) else {
                continue;
            };
            out.insert(name.clone(), raw.clone());
        }
        Ok(out)
    }
}

// Go: contentmapper/contentmapper.go:200 compilerOptionFields
// compilerOptionFields maps each CompilerOptions option name (its json tag) to its struct field index.
// PORT: Go reads the `json` tags by reflection and marshals each set field
// by itself. The port has no reflection: it marshals the whole options with
// the Go v2 struct marshaler (`options_json::CompilerOptionsJSON`, every member
// `omitzero`) and returns each written member's raw JSON by its tag name.
// A zero field writes no member, so it is absent here, as Go skips it with
// `field.IsZero()`. Each member value is the same JSON that Go's
// `json.Marshal(field.Interface())` writes.
fn compiler_option_fields(
    options: &CompilerOptions,
) -> Result<IndexMap<String, JsonValue>, GoError> {
    let text = json_marshal(&CompilerOptionsJSON(options), &[]).map_err(errors::from_value)?;
    let mut fields: IndexMap<String, JsonValue> = IndexMap::new();
    let mut dec = crate::frontend::json::json_new_decoder(text.as_bytes());
    unmarshal_struct_fields(&mut dec, "core.CompilerOptions", |name, dec| {
        let raw = dec.read_value()?;
        fields.insert(name.to_string(), JsonValue(raw.to_vec()));
        Ok(true)
    })
    .map_err(errors::from_value)?;
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Go: contentmapper/host_test.go:30 TestEmptyOutputExtensionsOverrideSurvivesSerialization (ts#64581)
    // PORT: the Go test is in `host_test.go`; it tests `Definition` only, so
    // it runs here on every platform (the host tests are Unix-only).
    #[test]
    fn test_empty_output_extensions_override_survives_serialization() {
        let definition = Definition {
            package: "mapper".to_string(),
            extensions: vec![".vue".to_string()],
            output_extensions: Some(BTreeMap::new()),
            ..Default::default()
        };
        let data = json_marshal(&definition, &[]).expect("marshal");
        let mut parsed = Definition::default();
        json_unmarshal(data.as_bytes(), &mut parsed, &[]).expect("unmarshal");
        assert!(parsed.output_extensions.is_some());
        let mut mapper = Mapper {
            definition: parsed,
            ..Default::default()
        };
        mapper.manifest.default_output_extensions =
            Some(BTreeMap::from([(".vue".to_string(), ".js".to_string())]));
        assert_eq!(
            mapper
                .definition
                .output_extensions
                .as_ref()
                .map_or(0, BTreeMap::len),
            0
        );
    }
}
