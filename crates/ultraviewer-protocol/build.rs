// Builds the vendored Zstandard 1.5.7 and WebRTC G.722 sources exactly as the
// macOS package does, and links the system TurboJPEG used for screen atlases.

fn main() {
    let zstd_sources = [
        "lib/common/debug.c", "lib/common/entropy_common.c",
        "lib/common/error_private.c", "lib/common/fse_decompress.c",
        "lib/common/pool.c", "lib/common/threading.c",
        "lib/common/xxhash.c", "lib/common/zstd_common.c",
        "lib/compress/fse_compress.c", "lib/compress/hist.c",
        "lib/compress/huf_compress.c", "lib/compress/zstd_compress.c",
        "lib/compress/zstd_compress_literals.c", "lib/compress/zstd_compress_sequences.c",
        "lib/compress/zstd_compress_superblock.c", "lib/compress/zstd_double_fast.c",
        "lib/compress/zstd_fast.c", "lib/compress/zstd_lazy.c",
        "lib/compress/zstd_ldm.c", "lib/compress/zstd_opt.c",
        "lib/compress/zstd_preSplit.c", "lib/compress/zstdmt_compress.c",
        "lib/decompress/huf_decompress.c", "lib/decompress/zstd_ddict.c",
        "lib/decompress/zstd_decompress.c", "lib/decompress/zstd_decompress_block.c",
        "lib/legacy/zstd_v04.c", "lib/legacy/zstd_v05.c",
        "lib/legacy/zstd_v06.c", "lib/legacy/zstd_v07.c",
    ];
    let mut zstd = cc::Build::new();
    for source in zstd_sources {
        zstd.file(format!("vendor/zstd/{source}"));
    }
    zstd.include("vendor/zstd/lib")
        .include("vendor/zstd/lib/common")
        .include("vendor/zstd/lib/legacy")
        .define("ZSTD_LEGACY_SUPPORT", "4")
        .define("ZSTD_DISABLE_ASM", "1")
        // Keep symbols private to this crate, matching an embedded copy.
        .define("ZSTDLIB_VISIBILITY", "")
        .define("ZSTDERRORLIB_VISIBILITY", "")
        .opt_level(3)
        .warnings(false)
        .compile("uvzstd");

    cc::Build::new()
        .file("vendor/g722/g722_decode.c")
        .file("vendor/g722/MacViewerG722.c")
        .include("vendor/g722/include")
        .opt_level(3)
        .warnings(false)
        .compile("uvg722");

    // TurboJPEG decodes the auxiliary atlas with the same library family as
    // the Windows client, so lossy output matches the native viewer.
    if pkg_config::Config::new().atleast_version("2.0").probe("libturbojpeg").is_err() {
        println!("cargo:rustc-link-lib=turbojpeg");
    }

    println!("cargo:rerun-if-changed=vendor");
    println!("cargo:rerun-if-changed=build.rs");
}
