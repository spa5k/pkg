//! Stable in-process performance measurements for the V1 discovery paths.

use std::hint::black_box;
use std::sync::LazyLock;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use pkg_core::{ChannelSequence, NixpkgsRevision, System};
use pkg_index::{BuildMetadata, BuiltIndex, IndexQuery, SearchOptions, build_index_from_json};

const FIXTURE: &[u8] = include_bytes!("../../../fixtures/nixpkgs-slice-tiny/index-input.json");

static BUILT_FIXTURE: LazyLock<BuiltIndex> = LazyLock::new(|| {
    build_index_from_json(metadata(), FIXTURE).expect("the committed fixture must build")
});

fn metadata() -> BuildMetadata {
    BuildMetadata::new(
        ChannelSequence::from_u64(42).expect("nonzero sequence"),
        System::Aarch64Darwin,
        NixpkgsRevision::new("0123456789abcdef0123456789abcdef01234567")
            .expect("valid fixture revision"),
        "2025-01-01T00:00:00Z",
    )
    .expect("valid fixture metadata")
}

fn v1_benches(criterion: &mut Criterion) {
    criterion.bench_function("index_build_tiny", |bencher| {
        bencher.iter(|| {
            black_box(
                build_index_from_json(metadata(), black_box(FIXTURE))
                    .expect("the committed fixture must build"),
            );
        });
    });

    let search = SearchOptions::new("ripgrep", 25, false, None).expect("valid fixed query");
    criterion.bench_function("search_ripgrep", |bencher| {
        bencher.iter(|| {
            black_box(
                IndexQuery::new(BUILT_FIXTURE.document(), false)
                    .search(black_box(&search))
                    .expect("the committed fixture must be searchable"),
            );
        });
    });

    criterion.bench_function("info_requests", |bencher| {
        bencher.iter(|| {
            black_box(
                IndexQuery::new(BUILT_FIXTURE.document(), false)
                    .info(black_box("python3Packages.requests"))
                    .expect("the committed fixture must contain requests"),
            );
        });
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(50)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = v1_benches
}
criterion_main!(benches);
