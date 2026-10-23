use super::*;

const ASSET: &str = "nightshift-linux-x86_64.deb";

fn release(tag: &str, prerelease: bool) -> Release {
    Release {
        tag_name: tag.into(),
        draft: false,
        prerelease,
        assets: [ASSET, CHECKSUMS]
            .map(|name| Asset { name: name.into() })
            .into(),
    }
}

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

#[test]
fn parses_release_channels_and_rejects_other_builds() {
    assert_eq!(version("0.2.0").channel(), Channel::Stable);
    assert_eq!(version("0.2.0-nightly.57").channel(), Channel::Nightly);
    for text in [
        "0.2.0-dev.3",
        "0.2",
        "0.2.0.1",
        "v0.2.0",
        "0.2.0-nightly.",
        "",
    ] {
        assert_eq!(Version::parse(text), None, "{text}");
    }
    assert!(version("0.10.0") > version("0.9.9"));
    assert!(version("0.2.0-nightly.10") > version("0.2.0-nightly.9"));
    assert!(version("0.3.0-nightly.1") > version("0.2.0-nightly.90"));
}

#[test]
fn nightly_builds_follow_only_newer_nightlies() {
    let releases = [
        release("v0.1.0-nightly.23", true),
        release("v0.1.0-nightly.25", true),
        release("v0.2.0", false),
        release("v0.1.0-nightly.21", true),
    ];
    let found = newest(version("0.1.0-nightly.23"), &releases, ASSET).unwrap();
    assert_eq!(found.tag_name, "v0.1.0-nightly.25");
    assert!(newest(version("0.1.0-nightly.25"), &releases, ASSET).is_none());
}

#[test]
fn stable_builds_follow_only_newer_stable_releases() {
    let releases = [
        release("v0.1.1", false),
        release("v0.2.0-nightly.30", true),
        release("v0.1.2", false),
        release("v0.1.0", false),
    ];
    let found = newest(version("0.1.1"), &releases, ASSET).unwrap();
    assert_eq!(found.tag_name, "v0.1.2");
}

#[test]
fn skips_drafts_mislabeled_releases_and_releases_missing_files() {
    let mut draft = release("v0.1.5", false);
    draft.draft = true;
    let mislabeled = release("v0.1.4", true);
    let mut no_package = release("v0.1.3", false);
    no_package.assets.retain(|asset| asset.name != ASSET);
    let mut no_checksums = release("v0.1.2", false);
    no_checksums.assets.retain(|asset| asset.name != CHECKSUMS);
    let releases = [draft, mislabeled, no_package, no_checksums];
    assert!(newest(version("0.1.0"), &releases, ASSET).is_none());
}

#[test]
fn reads_checksums_in_sha256sum_format() {
    let sums = "aaa  nightshift-macos-aarch64.dmg\nbbb *nightshift-linux-x86_64.deb\n";
    assert_eq!(expected_checksum(sums, ASSET), Some("bbb"));
    assert_eq!(
        expected_checksum(sums, "nightshift-macos-aarch64.dmg"),
        Some("aaa")
    );
    assert_eq!(
        expected_checksum(sums, "nightshift-linux-aarch64.deb"),
        None
    );
}

#[test]
fn stages_bundles_beside_the_installed_app() {
    let bundle = Path::new("/Applications/Nightshift.app");
    assert_eq!(
        sibling(bundle, "update"),
        Path::new("/Applications/.Nightshift.update.app")
    );
}
