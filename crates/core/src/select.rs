use std::collections::HashMap;

use crate::Mirror;

/// Pick the mirrors to be written, given a sorted list.
///
/// `overrides` maps URL to a manual choice: `Some(true)` forces inclusion,
/// `Some(false)` excludes. Everything else is auto-selected from the top until
/// `top_n` automatically chosen mirrors have been collected. The result keeps
/// the order of `sorted`.
pub fn select_indices(
    sorted: &[Mirror],
    top_n: usize,
    overrides: &HashMap<String, bool>,
) -> Vec<usize> {
    let mut auto = 0usize;
    let mut out = Vec::new();
    for (i, m) in sorted.iter().enumerate() {
        match overrides.get(&m.url) {
            Some(false) => {}
            Some(true) => out.push(i),
            None => {
                if auto < top_n {
                    auto += 1;
                    out.push(i);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StatusDocument;

    fn mirrors() -> Vec<Mirror> {
        StatusDocument::parse(include_str!("../tests/fixtures/status.json"))
            .unwrap()
            .urls
    }

    #[test]
    fn top_n_without_overrides() {
        let m = mirrors();
        assert_eq!(select_indices(&m, 3, &HashMap::new()), vec![0, 1, 2]);
        assert_eq!(select_indices(&m, 0, &HashMap::new()), Vec::<usize>::new());
    }

    #[test]
    fn excluded_are_skipped_and_replaced() {
        let m = mirrors();
        let mut o = HashMap::new();
        o.insert(m[0].url.clone(), false);
        assert_eq!(select_indices(&m, 2, &o), vec![1, 2]);
    }

    #[test]
    fn forced_do_not_consume_auto_slots() {
        let m = mirrors();
        let mut o = HashMap::new();
        o.insert(m[7].url.clone(), true);
        assert_eq!(select_indices(&m, 2, &o), vec![0, 1, 7]);
    }
}
