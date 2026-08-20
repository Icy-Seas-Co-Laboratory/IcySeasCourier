use courier_core::{PartRecord, PartStatus};
use uuid::Uuid;

/// Constraints shared by S3-compatible multipart implementations.
#[derive(Debug, Clone, Copy)]
pub struct MultipartLimits {
    pub target_part_size: u64,
    pub minimum_part_size: u64,
    pub maximum_part_size: u64,
    pub maximum_parts: u32,
}

impl Default for MultipartLimits {
    fn default() -> Self {
        Self {
            // Stay below Cloudflare's 100 MB request limit on Free and Pro plans.
            target_part_size: 64 * 1024 * 1024,
            minimum_part_size: 5 * 1024 * 1024,
            maximum_part_size: 5 * 1024 * 1024 * 1024,
            maximum_parts: 10_000,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlanError {
    #[error("multipart limits must use positive part sizes and part counts")]
    InvalidLimits,
    #[error("file size {file_size} exceeds multipart capacity {maximum_size}")]
    FileTooLarge { file_size: u64, maximum_size: u64 },
}

/// Creates stable, one-based source ranges. The final part may be smaller than
/// the S3 minimum. Empty files are represented by one zero-length part so the
/// transfer engine can handle them explicitly without losing provenance.
pub fn plan_parts(
    file_id: Uuid,
    file_size: u64,
    limits: MultipartLimits,
) -> Result<Vec<PartRecord>, PlanError> {
    if limits.minimum_part_size == 0
        || limits.maximum_parts == 0
        || limits.maximum_part_size < limits.minimum_part_size
    {
        return Err(PlanError::InvalidLimits);
    }
    let maximum_size = limits
        .maximum_part_size
        .saturating_mul(u64::from(limits.maximum_parts));
    if file_size > maximum_size {
        return Err(PlanError::FileTooLarge {
            file_size,
            maximum_size,
        });
    }

    let required = file_size.div_ceil(u64::from(limits.maximum_parts));
    let part_size = limits
        .target_part_size
        .max(limits.minimum_part_size)
        .max(required)
        .min(limits.maximum_part_size);

    if file_size == 0 {
        return Ok(vec![part(file_id, 1, 0, 0)]);
    }

    let count = file_size.div_ceil(part_size);
    Ok((0..count)
        .map(|index| {
            let offset = index * part_size;
            part(
                file_id,
                (index + 1) as u32,
                offset,
                (file_size - offset).min(part_size),
            )
        })
        .collect())
}

fn part(file_id: Uuid, part_number: u32, source_offset: u64, source_length: u64) -> PartRecord {
    PartRecord {
        file_id,
        part_number,
        source_offset,
        source_length,
        transport_length: None,
        checksum: None,
        etag: None,
        attempt_count: 0,
        status: PartStatus::Pending,
        last_error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_contiguous_parts_with_short_final_part() {
        let limits = MultipartLimits {
            target_part_size: 10,
            minimum_part_size: 5,
            maximum_part_size: 100,
            maximum_parts: 10,
        };
        let parts = plan_parts(Uuid::nil(), 25, limits).unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!((parts[2].source_offset, parts[2].source_length), (20, 5));
    }

    #[test]
    fn default_parts_fit_through_cloudflare_free_and_pro() {
        assert_eq!(
            MultipartLimits::default().target_part_size,
            64 * 1024 * 1024
        );
        assert!(MultipartLimits::default().target_part_size < 100_000_000);
    }

    #[test]
    fn increases_part_size_to_stay_below_part_limit() {
        let limits = MultipartLimits {
            target_part_size: 10,
            minimum_part_size: 5,
            maximum_part_size: 100,
            maximum_parts: 3,
        };
        let parts = plan_parts(Uuid::nil(), 25, limits).unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].source_length, 10);
    }

    #[test]
    fn preserves_empty_files() {
        let parts = plan_parts(Uuid::nil(), 0, MultipartLimits::default()).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].source_length, 0);
    }

    #[test]
    fn rejects_files_beyond_backend_capacity_without_panicking() {
        let limits = MultipartLimits {
            target_part_size: 5,
            minimum_part_size: 5,
            maximum_part_size: 10,
            maximum_parts: 2,
        };
        assert_eq!(
            plan_parts(Uuid::nil(), 21, limits),
            Err(PlanError::FileTooLarge {
                file_size: 21,
                maximum_size: 20
            })
        );
    }
}
