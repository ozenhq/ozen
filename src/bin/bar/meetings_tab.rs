//! The Meetings tab's cells, from `ozen meetings` rows (src/meetings.rs).

/// The Meetings table's cell in column `col` of `row`: When, Min, Lines, else the first words.
pub fn meeting_cell(meetings: &[Vec<String>], col: &str, row: usize) -> String {
    let i = match col {
        "When" => 1,
        "Min" => 2,
        "Lines" => 3,
        _ => 4,
    };
    let cell = meetings
        .get(row)
        .and_then(|m| m.get(i))
        .cloned()
        .unwrap_or_default();
    // a one-line meeting lasts no whole minute: `ozen meetings` says 0, the table says so honestly
    if i == 2 && cell == "0" {
        return "<1".into();
    }
    if i == 1
        && let Some(id) = meetings.get(row).and_then(|m| m.first())
        && let Some(recent) = recent_day(id, chrono::Local::now().date_naive())
    {
        return recent;
    }
    cell
}

/// A meeting from today or yesterday, by its id (start time in seconds): "Today 09:30", "Yesterday 21:12".
pub fn recent_day(id: &str, today: chrono::NaiveDate) -> Option<String> {
    let at = chrono::DateTime::from_timestamp(id.parse().ok()?, 0)?.with_timezone(&chrono::Local);
    let day = match (today - at.date_naive()).num_days() {
        0 => "Today",
        1 => "Yesterday",
        _ => return None,
    };
    Some(format!("{day} {}", at.format("%H:%M")))
}
