use std::path::{Path, PathBuf};

use failure::format_err;
use ignore::types::{Types, TypesBuilder};
use ignore::WalkBuilder;
use path_slash::PathExt; // Path::to_slash()
use serde::{Deserialize, Serialize};

use super::binding::Binding;
use super::filestem_from_path;
use super::plain_text::PlainText;
use super::text_blob::TextBlob;
use super::wasm_module::WasmModule;

use crate::settings::toml::{migrations::ApiMigration, DurableObjectsClass, KvNamespace};

use std::collections::HashMap;

#[derive(Debug)]
pub struct ServiceWorkerAssets {
    script_name: String,
    script_path: PathBuf,
    pub wasm_modules: Vec<WasmModule>,
    pub kv_namespaces: Vec<KvNamespace>,
    pub durable_object_classes: Vec<DurableObjectsClass>,
    pub text_blobs: Vec<TextBlob>,
    pub plain_texts: Vec<PlainText>,
}

impl ServiceWorkerAssets {
    pub fn new(
        script_path: PathBuf,
        wasm_modules: Vec<WasmModule>,
        kv_namespaces: Vec<KvNamespace>,
        durable_object_classes: Vec<DurableObjectsClass>,
        text_blobs: Vec<TextBlob>,
        plain_texts: Vec<PlainText>,
    ) -> Result<Self, failure::Error> {
        let script_name = filestem_from_path(&script_path).ok_or_else(|| {
            format_err!("filename should not be empty: {}", script_path.display())
        })?;

        Ok(Self {
            script_name,
            script_path,
            wasm_modules,
            kv_namespaces,
            durable_object_classes,
            text_blobs,
            plain_texts,
        })
    }

    pub fn bindings(&self) -> Vec<Binding> {
        let mut bindings = Vec::new();

        for wm in &self.wasm_modules {
            let binding = wm.binding();
            bindings.push(binding);
        }
        for kv in &self.kv_namespaces {
            let binding = kv.binding();
            bindings.push(binding);
        }
        for do_ns in &self.durable_object_classes {
            let binding = do_ns.binding();
            bindings.push(binding);
        }
        for blob in &self.text_blobs {
            let binding = blob.binding();
            bindings.push(binding);
        }
        for plain_text in &self.plain_texts {
            let binding = plain_text.binding();
            bindings.push(binding);
        }

        bindings
    }

    pub fn script_name(&self) -> String {
        self.script_name.to_string()
    }

    pub fn script_path(&self) -> PathBuf {
        self.script_path.clone()
    }
}

#[derive(Debug, PartialEq, PartialOrd, Eq, Ord)]
pub struct Module {
    pub name: String,
    pub path: PathBuf,
    pub module_type: ModuleType,
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Eq, Ord)]
pub enum ModuleType {
    ESModule,
    CommonJS,
    CompiledWasm,
    Text,
    Data,
}

impl ModuleType {
    pub fn content_type(&self) -> &str {
        match &self {
            Self::ESModule => "application/javascript+module",
            Self::CommonJS => "application/javascript",
            Self::CompiledWasm => "application/wasm",
            Self::Text => "text/plain",
            Self::Data => "application/octet-stream",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ModuleGlobs {
    esm: Option<Vec<String>>,
    cjs: Option<Vec<String>>,
    text: Option<Vec<String>>,
    data: Option<Vec<String>>,
    compiled_wasm: Option<Vec<String>>,
}

struct ModuleMatcher {
    matcher: Types,
    module_type: ModuleType,
}

impl ModuleGlobs {
    pub fn find_modules(&self, upload_dir: &Path) -> Result<Vec<Module>, failure::Error> {
        let (all_matcher, matchers) = self.build_type_matchers()?;

        let candidates_vec = WalkBuilder::new(upload_dir)
            .standard_filters(false)
            .follow_links(true)
            .types(all_matcher)
            .build()
            .collect::<Result<Vec<_>, _>>()?;
        let candidates = candidates_vec
            .iter()
            .filter(|e| e.path().is_file())
            .map(|e| e.path());

        Self::create_module_manifest(candidates, upload_dir, matchers.as_slice())
    }

    fn create_module_manifest<'a>(
        paths: impl Iterator<Item = &'a Path>,
        upload_dir: &'a Path,
        matchers: &'a [ModuleMatcher],
    ) -> Result<Vec<Module>, failure::Error> {
        let mut modules: HashMap<String, Module> = HashMap::new();

        for path in paths {
            let name = format!(
                "./{}",
                path.strip_prefix(upload_dir).map(|p| p.to_slash_lossy())?
            );
            for ModuleMatcher {
                matcher,
                module_type,
            } in matchers
            {
                if matcher.matched(path, false).is_whitelist() {
                    if modules.contains_key(&name) {
                        failure::bail!(
                            "The module at {} matched multiple module type globs.",
                            path.display()
                        );
                    } else {
                        modules.insert(
                            name.to_string(),
                            Module {
                                name: name.to_string(),
                                path: path.to_path_buf(),
                                module_type: *module_type,
                            },
                        );
                    }
                }
            }
        }

        Ok(modules.drain().map(|(_, m)| m).collect())
    }

    fn build_type_matchers(&self) -> Result<(Types, Vec<ModuleMatcher>), failure::Error> {
        let mut matchers = Vec::new();
        let mut all_builder = TypesBuilder::new();

        macro_rules! add_globs {
            ($name:ident, $module_type:ident) => {
                let empty_slice: &[&str] = &[];
                add_globs!($name, $module_type, empty_slice);
            };
            ($name:ident, $module_type:ident, $default_globs:expr) => {
                let mut builder = TypesBuilder::new();
                if let Some($name) = &self.$name {
                    for glob in $name {
                        all_builder.add(stringify!($module_type), &glob)?;
                        builder.add(stringify!($module_type), &glob)?;
                    }
                } else {
                    for glob in $default_globs {
                        all_builder.add(stringify!($module_type), glob)?;
                        builder.add(stringify!($module_type), glob)?;
                    }
                }
                builder.select("all");
                matchers.push(ModuleMatcher {
                    matcher: builder.build()?,
                    module_type: ModuleType::$module_type,
                });
            };
        }

        let mut add_all_globs = || -> Result<(), ignore::Error> {
            add_globs!(esm, ESModule, &["*.mjs"]);
            add_globs!(cjs, CommonJS, &["*.js", "*.cjs"]);
            add_globs!(compiled_wasm, CompiledWasm); // No default for non-standard wasm module type
            add_globs!(text, Text, &["*.txt"]);
            add_globs!(data, Data, &["*.bin"]); // TODO(now): Is this a good default?
            all_builder.select("all");
            Ok(())
        };

        match add_all_globs() {
            Ok(()) => (),
            Err(ignore::Error::Glob {
                glob: Some(glob),
                err,
            }) => failure::bail!(
                "encountered error while parsing the glob \"{}\": {}",
                glob,
                err
            ),
            Err(ignore::Error::Glob { glob: None, err }) => {
                failure::bail!("encountered error while parsing globs: {}", err)
            }
            Err(e) => failure::bail!(e),
        }

        Ok((all_builder.build()?, matchers))
    }
}

pub struct ModulesAssets {
    pub main_module: String,
    pub modules: Vec<Module>,
    pub kv_namespaces: Vec<KvNamespace>,
    pub durable_object_classes: Vec<DurableObjectsClass>,
    pub migration: Option<ApiMigration>,
    pub plain_texts: Vec<PlainText>,
}

impl ModulesAssets {
    pub fn new(
        main_module: String,
        modules: Vec<Module>,
        kv_namespaces: Vec<KvNamespace>,
        durable_object_classes: Vec<DurableObjectsClass>,
        migration: Option<ApiMigration>,
        plain_texts: Vec<PlainText>,
    ) -> Result<Self, failure::Error> {
        Ok(Self {
            main_module,
            modules,
            kv_namespaces,
            durable_object_classes,
            migration,
            plain_texts,
        })
    }

    pub fn bindings(&self) -> Vec<Binding> {
        let mut bindings = Vec::new();

        // Bindings that refer to a `part` of the uploaded files
        // in the service-worker format, are now modules.

        for kv in &self.kv_namespaces {
            let binding = kv.binding();
            bindings.push(binding);
        }
        for class in &self.durable_object_classes {
            let binding = class.binding();
            bindings.push(binding);
        }
        for plain_text in &self.plain_texts {
            let binding = plain_text.binding();
            bindings.push(binding);
        }

        bindings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_module_parsing() -> Result<(), failure::Error> {
        use super::ModuleType::*;

        let upload_dir = Path::new("/worker/dist");

        let fs = &[
            (
                "/worker/dist/foo/bar/index.mjs",
                "./foo/bar/index.mjs",
                ESModule,
            ),
            ("/worker/dist/bar.js", "./bar.js", CommonJS),
            ("/worker/dist/foo/baz.cjs", "./foo/baz.cjs", CommonJS),
            ("/worker/dist/wat.txt", "./wat.txt", Text),
            ("/worker/dist/wat.bin", "./wat.bin", Data),
        ];

        let paths = fs.iter().map(|m| Path::new(m.0));
        let mut modules = fs
            .iter()
            .map(|m| Module {
                path: PathBuf::from(m.0),
                name: m.1.to_string(),
                module_type: m.2,
            })
            .collect::<Vec<_>>();
        let globs: ModuleGlobs = ModuleGlobs::default();
        let (_, matchers) = globs.build_type_matchers()?;

        let mut manifest = ModuleGlobs::create_module_manifest(paths, upload_dir, &matchers)?;

        modules.sort();
        manifest.sort();

        println!("{:#?}", manifest);

        assert!(manifest.iter().eq(modules.iter()));

        Ok(())
    }
}
