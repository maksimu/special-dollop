use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

/// Scalar implementation (baseline)
fn fix_alpha_scalar(data: &mut [u8]) {
    for chunk in data.chunks_exact_mut(4) {
        chunk[3] = 255;
    }
}

/// Old implementation (with Vec allocation)
fn fix_alpha_old(data: &[u8]) -> Vec<u8> {
    let mut fixed = data.to_vec();
    for i in (0..fixed.len()).step_by(4) {
        if i + 3 < fixed.len() {
            fixed[i + 3] = 255;
        }
    }
    fixed
}

fn benchmark_alpha_fix(c: &mut Criterion) {
    let mut group = c.benchmark_group("alpha_fix");

    // Test different image sizes
    let sizes = vec![
        ("640x480", 640 * 480 * 4),
        ("1024x768", 1024 * 768 * 4),
        ("1920x1080", 1920 * 1080 * 4),
        ("3840x2160", 3840 * 2160 * 4),
    ];

    for (name, size) in sizes {
        let mut data = vec![0u8; size];

        // Benchmark old implementation (with allocation)
        group.bench_with_input(
            BenchmarkId::new("old_with_alloc", name),
            &size,
            |b, &size| {
                let data = vec![0u8; size];
                b.iter(|| {
                    let result = fix_alpha_old(black_box(&data));
                    black_box(result);
                });
            },
        );

        // Benchmark new scalar implementation (in-place)
        group.bench_with_input(BenchmarkId::new("scalar_inplace", name), &size, |b, _| {
            b.iter(|| {
                fix_alpha_scalar(black_box(&mut data));
            });
        });

        // Benchmark SIMD implementation (if available)
        #[cfg(target_arch = "aarch64")]
        {
            if std::arch::is_aarch64_feature_detected!("neon") {
                group.bench_with_input(BenchmarkId::new("neon_inplace", name), &size, |b, _| {
                    b.iter(|| {
                        unsafe { fix_alpha_neon(black_box(&mut data)) };
                    });
                });
            }
        }

        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            if is_x86_feature_detected!("avx2") {
                group.bench_with_input(BenchmarkId::new("avx2_inplace", name), &size, |b, _| {
                    b.iter(|| {
                        unsafe { fix_alpha_avx2(black_box(&mut data)) };
                    });
                });
            }
        }
    }

    group.finish();
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn fix_alpha_neon(data: &mut [u8]) {
    use std::arch::aarch64::*;

    let len = data.len();
    let mut i = 0;

    while i + 16 <= len {
        let pixels = vld1q_u8(data.as_ptr().add(i));
        let alpha_fixed = vsetq_lane_u8(0xFF, pixels, 3);
        let alpha_fixed = vsetq_lane_u8(0xFF, alpha_fixed, 7);
        let alpha_fixed = vsetq_lane_u8(0xFF, alpha_fixed, 11);
        let alpha_fixed = vsetq_lane_u8(0xFF, alpha_fixed, 15);
        vst1q_u8(data.as_mut_ptr().add(i), alpha_fixed);
        i += 16;
    }

    fix_alpha_scalar(&mut data[i..]);
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn fix_alpha_avx2(data: &mut [u8]) {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let len = data.len();
    let mut i = 0;

    let alpha_mask = _mm256_set_epi8(
        -1, 0, 0, 0, -1, 0, 0, 0, -1, 0, 0, 0, -1, 0, 0, 0, -1, 0, 0, 0, -1, 0, 0, 0, -1, 0, 0, 0,
        -1, 0, 0, 0,
    );
    let alpha_value = _mm256_set1_epi8(-1);

    while i + 32 <= len {
        let pixels = _mm256_loadu_si256(data.as_ptr().add(i) as *const __m256i);
        let fixed = _mm256_blendv_epi8(pixels, alpha_value, alpha_mask);
        _mm256_storeu_si256(data.as_mut_ptr().add(i) as *mut __m256i, fixed);
        i += 32;
    }

    fix_alpha_scalar(&mut data[i..]);
}

criterion_group!(benches, benchmark_alpha_fix);
criterion_main!(benches);
