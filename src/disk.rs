//! Free disk space, for `ozen health`: warn while there's still room, before recording and
//! transcription start failing on a full disk.
use std::process::Command;

const LOW_GB: f64 = 2.0; // ~15 min of chunks is ~0.1 GB, so this leaves hours, but other apps fill disks fast

/// Free GB on the disk holding the working directory, when it is below `LOW_GB`.
pub fn low() -> Option<f64> {
    let out = Command::new("df").args(["-k", "."]).output().ok()?;
    free_gb(&String::from_utf8_lossy(&out.stdout)).filter(|gb| *gb < LOW_GB)
}

/// Free space in GB from `df -k` output (second line, fourth column: available 1K blocks).
fn free_gb(df: &str) -> Option<f64> {
    let kb: f64 = df.lines().nth(1)?.split_whitespace().nth(3)?.parse().ok()?;
    Some(kb / 1024.0 / 1024.0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_free_space_from_df() {
        let df = "Filesystem 1024-blocks Used Available Capacity iused ifree %iused Mounted on\n\
                  /dev/disk3s1s1 482797652 20046436 5347328 79% 459k 54M 1% /\n";
        let gb = super::free_gb(df).unwrap();
        assert!((gb - 5.1).abs() < 0.01, "{gb}");
        assert_eq!(super::free_gb(""), None);
    }
}
