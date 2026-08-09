//! Regenerate the committed production-client contract fixture.

use std::path::Path;

use pkg_spike_s2_tough::build_fixture;

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[tokio::main]
async fn main() {
    let fixture = build_fixture().await;
    let destination = Path::new("../../fixtures/channel-v1");
    std::fs::create_dir_all(destination).unwrap();
    std::fs::write(destination.join("root.json"), fixture.root_bytes()).unwrap();
    copy_tree(&fixture.repo.metadata_dir, &destination.join("metadata"));
    copy_tree(&fixture.repo.targets_dir, &destination.join("targets"));
}
