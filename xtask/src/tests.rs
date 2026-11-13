use super::valid_package_name;

#[test]
fn package_names_are_one_cargo_package_name() {
    for package in ["gate", "windows-guest", "crate_2", "A1"] {
        assert!(
            valid_package_name(package),
            "{package:?} is one package name"
        );
    }
    for package in ["", "gate other", "../gate", "gate/package"] {
        assert!(
            !valid_package_name(package),
            "{package:?} is not one package name"
        );
    }
}
