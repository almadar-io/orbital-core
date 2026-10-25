#[cfg(not(feature = "std"))]
use std::prelude::v1::*;
#[allow(unused_imports)]
use orbital_core_base::language_data::*;

use orbital_core_base::language_data as base_language_data;

fn implemented(name: &str) -> bool {
    crate::evaluator::get_operator(name).is_some()
}

/// Refuses an operator table naming a rust-target operator this evaluator does not implement.
pub fn certify(data: &OperatorData) -> Result<(), LanguageDataError> {
    base_language_data::certify_operator_data(data, implemented)
}

/// The installed table's certification, run once: the installed table never changes after it loads.
fn certify_installed(data: &'static OperatorData) -> Result<(), LanguageDataError> {
    static CERTIFIED: orbital_core_base::sync::OnceLock<Result<(), LanguageDataError>> = orbital_core_base::sync::OnceLock::new();
    CERTIFIED.get_or_init(|| certify(data)).clone()
}

pub fn preflight() -> Result<(), LanguageDataError> {
    base_language_data::preflight()?;
    certify_installed(base_language_data::operator_data()?)
}

pub fn operator_data() -> Result<&'static OperatorData, LanguageDataError> {
    let data = base_language_data::operator_data()?;
    certify_installed(data)?;
    Ok(data)
}

pub fn operator_data_after_preflight() -> &'static OperatorData {
    match operator_data() {
        Ok(data) => data,
        Err(e) => panic!(
            "{}: {e} (the entry point must call operator_data() first)",
            e.code()
        ),
    }
}

#[cfg(feature = "std")]
pub fn load_operator_data(
    start_dir: &std::path::Path,
    user_store: &std::path::Path,
) -> Result<OperatorData, LanguageDataError> {
    let data = base_language_data::load_operator_data(start_dir, user_store)?;
    certify(&data)?;
    Ok(data)
}

#[cfg(feature = "std")]
pub fn load_operator_data_for(
    start_dir: &std::path::Path,
    user_store: &std::path::Path,
    running: &str,
) -> Result<OperatorData, LanguageDataError> {
    let data = base_language_data::load_operator_data_for(start_dir, user_store, running)?;
    certify(&data)?;
    Ok(data)
}

pub fn parse_language_bundle(
    bundle: &str,
    running: &str,
) -> Result<
    (
        LanguageData,
        OperatorData,
        Vec<orbital_core_base::i18n::packs::VocabularyPack>,
    ),
    LanguageDataError,
> {
    let parsed = base_language_data::parse_language_bundle(bundle, running)?;
    certify(&parsed.1)?;
    Ok(parsed)
}

pub fn install_language_bundle(bundle: &str) -> Result<(), LanguageDataError> {
    let (language, ops, packs) = parse_language_bundle(bundle, running_version())?;
    let _ = install_language_data(language);
    let _ = install_operator_data(ops);
    orbital_core_base::i18n::packs::install_vocabulary_packs(packs)?;
    Ok(())
}
