//! Queue order: waiting rows read top-down in the order they'll start. Drag a row's grip,
//! ↑ / ↓ on it, Ctrl+↑ / Ctrl+↓ or "Move to top / bottom" in its menu. Running and finished
//! rows stay put; a playlist card moves with its rows, and rows move inside their card.
//! The list is the truth: after a change the queue is told the order it shows.

use slint::{ComponentHandle, Model};

use crate::{AppWindow, JobItem, QueueActions, rows};

/// Waiting or paused: its place in the list decides when it starts.
fn waiting(j: &JobItem) -> bool {
    !j.header && (j.state == 0 || j.state == 6)
}

/// Rows that move together, as `start..end` ranges of `list`: at the top level a card with
/// its rows or a single row; inside card `group`, each of its rows.
fn units(list: &[JobItem], inside: Option<i32>) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < list.len() {
        let j = &list[i];
        match inside {
            Some(g) => {
                if j.child && !j.header && j.group == g {
                    out.push((i, i + 1));
                }
                i += 1;
            }
            None if j.header => {
                let mut end = i + 1;
                while end < list.len() && list[end].child && !list[end].header && list[end].group == j.group {
                    end += 1;
                }
                out.push((i, end));
                i = end;
            }
            None => {
                if !j.child {
                    out.push((i, i + 1));
                }
                i += 1;
            }
        }
    }
    out
}

fn movable(list: &[JobItem], unit: (usize, usize)) -> bool {
    list[unit.0..unit.1].iter().any(waiting)
}

/// Where a new top-level row goes: below the last waiting one, so the list keeps reading in
/// start order (the queue starts new jobs after everything already waiting). Top otherwise.
pub(crate) fn insert_at(list: &[JobItem]) -> usize {
    units(list, None)
        .into_iter()
        .filter(|&u| list[u.0..u.1].iter().any(|j| !j.header && j.state == 0))
        .map(|u| u.1)
        .next_back()
        .unwrap_or(0)
}

/// Move the row `id` (a card: the group, negated) by `steps` places among the rows that can
/// move; a big number goes to the top or bottom.
fn move_row(id: i32, steps: i32) {
    let model = rows();
    let mut list: Vec<JobItem> = model.iter().collect();
    let (pos, inside) = if id < 0 {
        match list.iter().position(|j| j.header && j.group == -id) {
            Some(p) => (p, None),
            None => return,
        }
    } else {
        match list.iter().position(|j| !j.header && j.id == id) {
            Some(p) => (p, if list[p].child { Some(list[p].group) } else { None }),
            None => return,
        }
    };
    let all = units(&list, inside);
    let free: Vec<(usize, usize)> = all.into_iter().filter(|&u| movable(&list, u)).collect();
    let Some(k) = free.iter().position(|u| u.0 == pos) else { return };
    let t = (k as i64 + steps as i64).clamp(0, free.len() as i64 - 1) as usize;
    if t == k {
        return;
    }
    let (start, end) = free[k];
    let taken: Vec<JobItem> = list.drain(start..end).collect();
    let n = taken.len();
    // Before the target (moving up) or after it (moving down), in the shortened list.
    let at = if t < k { free[t].0 } else { free[t].1 - n };
    for (i, row) in taken.into_iter().enumerate() {
        list.insert(at + i, row);
    }
    model.set_vec(list);
    sync();
    refresh_movable();
}

/// Tell the queue the start order the list shows.
pub(crate) fn sync() {
    let Some(queue) = crate::queue() else { return };
    let order: Vec<u64> = rows().iter().filter(waiting).map(|j| j.id as u64).collect();
    queue.reorder(&order);
}

/// Give rows that can move (another movable row next to them) their grip.
pub(crate) fn refresh_movable() {
    let model = rows();
    let list: Vec<JobItem> = model.iter().collect();
    let mut can = vec![false; list.len()];
    let mark = |units: Vec<(usize, usize)>, can: &mut Vec<bool>| {
        let free: Vec<(usize, usize)> = units.into_iter().filter(|&u| movable(&list, u)).collect();
        if free.len() >= 2 {
            for u in free {
                can[u.0] = true;
            }
        }
    };
    mark(units(&list, None), &mut can);
    let cards: Vec<i32> = list.iter().filter(|j| j.header).map(|j| j.group).collect();
    for g in cards {
        mark(units(&list, Some(g)), &mut can);
    }
    for (i, mut row) in list.into_iter().enumerate() {
        // A card's grip moves the card, so only waiting rows of it get their own.
        if row.can_move != can[i] {
            row.can_move = can[i];
            model.set_row_data(i, row);
        }
    }
}

pub(crate) fn setup(ui: &AppWindow) {
    ui.global::<QueueActions>().on_move(move_row);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i32, group: i32, state: i32, header: bool, child: bool) -> JobItem {
        JobItem { id, group, state, header, child, ..Default::default() }
    }

    #[test]
    fn cards_move_with_their_rows() {
        let list = vec![
            row(1, 0, 1, false, false),
            row(-5, 5, 0, true, false),
            row(6, 5, 0, false, true),
            row(7, 5, 0, false, true),
            row(2, 0, 3, false, false),
            row(3, 0, 0, false, false),
        ];
        assert_eq!(units(&list, None), [(0, 1), (1, 4), (4, 5), (5, 6)]);
        assert_eq!(units(&list, Some(5)), [(2, 3), (3, 4)]);
        // New rows go below the last waiting one.
        assert_eq!(insert_at(&list), 6);
        assert_eq!(insert_at(&list[..5]), 4);
        assert_eq!(insert_at(&list[..1]), 0);
    }
}
