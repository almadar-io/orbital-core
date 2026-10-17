#[allow(unused_imports)]
use crate::language_loader::install_language_bundle;
#[allow(unused_imports)]
use crate::language_loader::load_operator_data;
#[allow(unused_imports)]
use crate::language_loader::operator_data;
#[allow(unused_imports)]
use crate::language_loader::operator_data_after_preflight;
#[allow(unused_imports)]
use crate::language_loader::preflight;
#[allow(unused_imports)]
use orbital_core_base::language_data::*;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

#[cfg(test)]
mod tests {
    use super::*;
    use orbital_core_base::behavior_packages::discover;
    use serde_json::json;

    use crate::language_loader::{load_operator_data_for, parse_language_bundle};

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "orb-language-data-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Tmp(dir)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn install_core(root: &Path, version: &str, compiler: &str) -> PathBuf {
        let dir = root.join("node_modules/@almadar/core");
        fs::create_dir_all(dir.join("dist/patterns")).unwrap();
        fs::write(
            dir.join("package.json"),
            json!({
                "name": "@almadar/core",
                "version": version,
                "orbLanguage": {
                    "compiler": compiler,
                    "patterns": "dist/patterns/patterns-registry.json",
                    "componentMapping": "dist/patterns/component-mapping.json",
                    "eventContracts": "dist/patterns/event-contracts.json",
                    "services": "dist/patterns/services-registry.json",
                    "bindingRules": "dist/language/binding-rules.json",
                    "programTypes": "dist/language/program-types.json",
                    "themes": "themes"
                }
            })
            .to_string(),
        )
        .unwrap();
        for (file, body) in [
            ("patterns-registry.json", r#"{"patterns":{"p":{}}}"#),
            ("component-mapping.json", r#"{"mappings":{}}"#),
            ("event-contracts.json", r#"{"contracts":{}}"#),
            ("services-registry.json", r#"{"services":{}}"#),
        ] {
            fs::write(dir.join("dist/patterns").join(file), body).unwrap();
        }
        fs::create_dir_all(dir.join("dist/language")).unwrap();
        fs::write(
            dir.join("dist/language/binding-rules.json"),
            r#"{"guard":{"allowed":["zz"]}}"#,
        )
        .unwrap();
        fs::write(
            dir.join("dist/language/program-types.json"),
            r#"{"OrbitalValue":{"type":"object","properties":{"behavior":{"type":"string","required":true}}}}"#,
        )
        .unwrap();
        fs::create_dir_all(dir.join("themes")).unwrap();
        fs::write(
            dir.join("themes/zz-theme.json"),
            r#"{"name":"zz-theme","tokens":{}}"#,
        )
        .unwrap();
        dir
    }

    #[test]
    fn installed_core_resolves_and_reads_all_four_files() {
        let tmp = Tmp::new("ok");
        install_core(&tmp.0, "10.1.0", ">=17.0.0");
        let data = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap();
        assert_eq!(data.core_version, "10.1.0");
        assert!(data.patterns.contains("\"p\""));
        assert!(data.component_mapping.contains("mappings"));
        assert!(data.event_contracts.contains("contracts"));
        assert!(data.services.contains("services"));
        assert!(data.binding_rules.contains("\"zz\""));
        assert!(data.program_types.contains("OrbitalValue"));
        assert_eq!(data.themes.len(), 1);
        assert!(data.themes[0].contains("zz-theme"));
    }

    #[test]
    fn edge_a_core_without_program_types_or_with_a_malformed_table_is_invalid() {
        let tmp = Tmp::new("no-program-types");
        let dir = install_core(&tmp.0, "10.1.0", ">=17.0.0");
        fs::write(
            dir.join("dist/language/program-types.json"),
            r#"{"OrbitalValue":{"type":"nonsense"}}"#,
        )
        .unwrap();
        let err = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INVALID");
        assert!(err.to_string().contains("OrbitalValue"), "{err}");
        fs::remove_file(dir.join("dist/language/program-types.json")).unwrap();
        let err = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INVALID");
    }

    #[test]
    fn edge_a_core_without_binding_rules_is_invalid_naming_the_key() {
        let tmp = Tmp::new("no-binding-rules");
        let dir = install_core(&tmp.0, "10.1.0", ">=17.0.0");
        fs::remove_file(dir.join("dist/language/binding-rules.json")).unwrap();
        let err = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INVALID");
    }

    #[test]
    fn edge_a_theme_directory_without_presets_is_invalid() {
        let tmp = Tmp::new("no-themes");
        let dir = install_core(&tmp.0, "10.1.0", ">=17.0.0");
        fs::remove_file(dir.join("themes/zz-theme.json")).unwrap();
        let err = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INVALID");
        assert!(err.to_string().contains("no `.json` files"), "{err}");
    }

    #[test]
    fn resolves_from_a_nested_start_dir_by_walking_up() {
        let tmp = Tmp::new("walk");
        install_core(&tmp.0, "10.1.0", ">=17.0.0");
        let nested = tmp.0.join("a/b/c");
        fs::create_dir_all(&nested).unwrap();
        assert!(load_language_data_for(&nested, &tmp.0.join("store"), "17.12.0").is_ok());
    }

    #[test]
    fn user_store_is_the_last_rung() {
        let tmp = Tmp::new("store");
        let store = tmp.0.join("store");
        install_core(&store, "9.0.0", ">=17.0.0");
        let project = tmp.0.join("project");
        fs::create_dir_all(&project).unwrap();
        let data = load_language_data_for(&project, &store, "17.12.0").unwrap();
        assert_eq!(data.core_version, "9.0.0");
        install_core(&project, "10.0.0", ">=17.0.0");
        let data = load_language_data_for(&project, &store, "17.12.0").unwrap();
        assert_eq!(
            data.core_version, "10.0.0",
            "project copy shadows the store"
        );
    }

    #[test]
    fn missing_core_is_not_installed_with_the_install_hint() {
        let tmp = Tmp::new("missing");
        let err = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(
            err,
            LanguageDataError::NotInstalled {
                package: CORE_PACKAGE
            }
        );
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_NOT_INSTALLED");
        assert!(err.needs_install());
        let msg = err.to_string();
        assert!(msg.contains("@almadar/core is not installed"), "{msg}");
        assert!(msg.contains("run `orb update`"), "{msg}");
    }

    #[test]
    fn compiler_range_mismatch_is_incompatible_naming_both_versions() {
        let tmp = Tmp::new("range");
        install_core(&tmp.0, "10.1.0", ">=17.12.0");
        let err = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.11.2").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INCOMPATIBLE");
        assert!(!err.needs_install());
        let msg = err.to_string();
        assert!(
            msg.contains("10.1.0") && msg.contains("17.11.2") && msg.contains(">=17.12.0"),
            "{msg}"
        );
    }

    #[test]
    fn edge_missing_language_block_and_missing_data_file_are_invalid_not_empty() {
        let tmp = Tmp::new("invalid");
        let dir = install_core(&tmp.0, "10.1.0", ">=17.0.0");
        fs::remove_file(dir.join("dist/patterns/services-registry.json")).unwrap();
        let err = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INVALID");

        fs::write(
            dir.join("package.json"),
            r#"{"name":"@almadar/core","version":"1.0.0","orb":{"compiler":">=1.0.0","language":{}}}"#,
        )
        .unwrap();
        let err = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert!(err.to_string().contains("orbLanguage"), "{err}");
    }

    #[test]
    fn edge_bad_compiler_range_is_invalid() {
        let tmp = Tmp::new("badrange");
        install_core(&tmp.0, "10.1.0", "not a range");
        let err = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INVALID");
    }

    #[test]
    fn core_language_block_is_not_mistaken_for_a_behavior_package() {
        let tmp = Tmp::new("not-behavior");
        install_core(&tmp.0, "10.1.0", ">=17.0.0");
        let found =
            discover(&tmp.0, &tmp.0.join("store")).expect("core must not make discovery fail");
        assert!(
            found.is_empty(),
            "core is language data, not a behavior package: {found:?}"
        );
    }

    #[test]
    fn edge_a_malformed_behavior_block_still_errors_loudly() {
        let tmp = Tmp::new("malformed-behavior");
        let dir = tmp.0.join("node_modules/@x/broken");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("package.json"),
            r#"{"name":"@x/broken","version":"1.0.0","orb":{"registry":"registry"}}"#,
        )
        .unwrap();
        assert!(discover(&tmp.0, &tmp.0.join("store")).is_err());
    }

    fn install_std(root: &Path, compiler: &str, table: &str) -> PathBuf {
        let dir = root.join("node_modules/@almadar/std");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("package.json"),
            json!({
                "name": "@almadar/std",
                "version": "8.2.0",
                "orbLanguage": { "compiler": compiler, "operators": "canonical-operators.json" }
            })
            .to_string(),
        )
        .unwrap();
        fs::write(dir.join("canonical-operators.json"), table).unwrap();
        dir
    }

    fn table(op: &str, category_targets: &str) -> String {
        format!(
            r#"{{"categories":{{"arithmetic":{{"target":["rust","ts"]}},"other":{{"target":{category_targets}}}}},
                "operators":{{"+":{{"category":"arithmetic"}},"{op}":{{"category":"other"}}}}}}"#
        )
    }

    #[test]
    fn installed_std_operator_table_resolves() {
        let tmp = Tmp::new("ops-ok");
        install_std(&tmp.0, ">=17.0.0", &table("-", r#"["rust"]"#));
        let data = load_operator_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap();
        assert_eq!(data.std_version, "8.2.0");
        assert!(data.operators.contains("arithmetic"));
    }

    #[test]
    fn missing_std_is_not_installed_naming_std() {
        let tmp = Tmp::new("ops-missing");
        install_core(&tmp.0, "10.1.0", ">=17.0.0");
        let err = load_operator_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(
            err,
            LanguageDataError::NotInstalled {
                package: STD_PACKAGE
            }
        );
        assert_eq!(err.code(), "ORB_BEHAVIOR_PACKAGE_NOT_INSTALLED");
        assert!(err.needs_install());
        let msg = err.to_string();
        assert!(msg.contains("@almadar/std is not installed"), "{msg}");
        assert!(msg.contains("run `orb update`"), "{msg}");
    }

    #[test]
    fn std_range_mismatch_is_incompatible_naming_std() {
        let tmp = Tmp::new("ops-range");
        install_std(&tmp.0, ">=18.0.0", &table("-", r#"["rust"]"#));
        let err = load_operator_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INCOMPATIBLE");
        let msg = err.to_string();
        assert!(
            msg.contains("@almadar/std@8.2.0") && msg.contains(">=18.0.0"),
            "{msg}"
        );
    }

    #[test]
    fn a_rust_target_operator_the_evaluator_lacks_is_refused() {
        let tmp = Tmp::new("ops-unimpl");
        install_std(
            &tmp.0,
            ">=17.0.0",
            &table("zz/not-implemented", r#"["rust"]"#),
        );
        let err = load_operator_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INCOMPATIBLE");
        assert!(!err.needs_install());
        assert!(err.to_string().contains("zz/not-implemented"), "{err}");
    }

    #[test]
    fn control_the_same_operator_outside_a_rust_category_is_accepted() {
        let tmp = Tmp::new("ops-ts-only");
        install_std(
            &tmp.0,
            ">=17.0.0",
            &table("zz/not-implemented", r#"["ts"]"#),
        );
        assert!(load_operator_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").is_ok());
    }

    #[test]
    fn edge_malformed_table_and_missing_operators_field_are_invalid() {
        let tmp = Tmp::new("ops-invalid");
        let dir = install_std(&tmp.0, ">=17.0.0", r#"{"operators":{}}"#);
        let err = load_operator_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INVALID");
        assert!(err.to_string().contains("categories"), "{err}");

        fs::write(
            dir.join("package.json"),
            r#"{"name":"@almadar/std","version":"8.2.0","orbLanguage":{"compiler":">=17.0.0"}}"#,
        )
        .unwrap();
        let err = load_operator_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert!(err.to_string().contains("orbLanguage.operators"), "{err}");
    }

    fn bundle(compiler: &str, extra_op: &str) -> String {
        json!({
            "core": {
                "version": "10.1.0", "compiler": compiler,
                "patterns": {"patterns": {"p": {}}}, "componentMapping": {"mappings": {}},
                "eventContracts": {"contracts": {}}, "services": {"services": {}},
                "bindingRules": {"guard": {"allowed": ["zz"]}},
                "programTypes": {"OrbitalValue": {"type": "object", "properties": {"behavior": {"type": "string", "required": true}}}},
                "themes": [{"name": "zz", "tokens": {}}]
            },
            "std": {
                "version": "8.2.0", "compiler": compiler,
                "operators": serde_json::from_str::<Value>(&table(extra_op, r#"["rust"]"#)).unwrap()
            },
            "vocabularies": [{
                "package": "lolo-zz", "version": "1.0.0", "compiler": compiler,
                "vocabulary": {"meta": {"code": "zz", "name": "Zz", "rtl": false},
                               "keywords": {"orbital": "zz-orbital"}}
            }]
        })
        .to_string()
    }

    #[test]
    fn a_language_bundle_carries_what_the_native_loaders_read() {
        let (lang, ops, packs) =
            parse_language_bundle(&bundle(">=17.0.0", "-"), "17.12.0").unwrap();
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].definition.keywords["orbital"], "zz-orbital");
        assert_eq!(lang.core_version, "10.1.0");
        assert!(lang.patterns.contains("\"p\""));
        assert!(lang.binding_rules.contains("\"zz\""));
        assert!(lang.program_types.contains("OrbitalValue"));
        assert_eq!(lang.themes.len(), 1);
        assert_eq!(ops.std_version, "8.2.0");
        assert!(ops.operators.contains("arithmetic"));
    }

    #[test]
    fn a_language_bundle_gets_the_same_checks() {
        let err = parse_language_bundle(&bundle(">=18.0.0", "-"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INCOMPATIBLE");
        let err = parse_language_bundle(&bundle(">=17.0.0", "zz/not-implemented"), "17.12.0")
            .unwrap_err();
        assert!(err.to_string().contains("zz/not-implemented"), "{err}");
    }

    #[test]
    fn edge_a_bundle_missing_a_part_or_themes_is_invalid() {
        let err = parse_language_bundle(r#"{"core":{}}"#, "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INVALID");
        let mut v: Value = serde_json::from_str(&bundle(">=17.0.0", "-")).unwrap();
        v["core"]["themes"] = json!([]);
        let err = parse_language_bundle(&v.to_string(), "17.12.0").unwrap_err();
        assert!(err.to_string().contains("core.themes"), "{err}");
        let mut v: Value = serde_json::from_str(&bundle(">=17.0.0", "-")).unwrap();
        v.as_object_mut().unwrap().remove("vocabularies");
        let err = parse_language_bundle(&v.to_string(), "17.12.0").unwrap_err();
        assert!(err.to_string().contains("vocabularies"), "{err}");
        let mut v: Value = serde_json::from_str(&bundle(">=17.0.0", "-")).unwrap();
        v["vocabularies"][0]["compiler"] = json!(">=18.0.0");
        let err = parse_language_bundle(&v.to_string(), "17.12.0").unwrap_err();
        assert!(err.to_string().contains("lolo-zz"), "{err}");
        let mut v: Value = serde_json::from_str(&bundle(">=17.0.0", "-")).unwrap();
        v["core"].as_object_mut().unwrap().remove("bindingRules");
        let err = parse_language_bundle(&v.to_string(), "17.12.0").unwrap_err();
        assert!(err.to_string().contains("core.bindingRules"), "{err}");
        let mut v: Value = serde_json::from_str(&bundle(">=17.0.0", "-")).unwrap();
        v["core"].as_object_mut().unwrap().remove("programTypes");
        let err = parse_language_bundle(&v.to_string(), "17.12.0").unwrap_err();
        assert!(err.to_string().contains("core.programTypes"), "{err}");
        let mut v: Value = serde_json::from_str(&bundle(">=17.0.0", "-")).unwrap();
        v["core"]["programTypes"] = json!({"OrbitalValue": {"type": "nonsense"}});
        let err = parse_language_bundle(&v.to_string(), "17.12.0").unwrap_err();
        assert!(err.to_string().contains("OrbitalValue"), "{err}");
    }
}
