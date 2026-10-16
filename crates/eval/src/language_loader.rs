#[allow(unused_imports)]
use orbital_core_base::language_data::*;

use orbital_core_base::language_data as base_language_data;

pub fn install_evaluator() {
    base_language_data::install_operator_probe(|name| crate::evaluator::get_operator(name).is_some());
}

pub fn preflight() -> Result<(), LanguageDataError> {
    install_evaluator();
    base_language_data::preflight()
}

pub fn operator_data() -> Result<&'static OperatorData, LanguageDataError> {
    install_evaluator();
    base_language_data::operator_data()
}

pub fn operator_data_after_preflight() -> &'static OperatorData {
    install_evaluator();
    base_language_data::operator_data_after_preflight()
}

pub fn load_operator_data(
    start_dir: &std::path::Path,
    user_store: &std::path::Path,
) -> Result<OperatorData, LanguageDataError> {
    install_evaluator();
    base_language_data::load_operator_data(start_dir, user_store)
}

pub fn install_language_bundle(bundle: &str) -> Result<(), LanguageDataError> {
    install_evaluator();
    base_language_data::install_language_bundle(bundle)
}
