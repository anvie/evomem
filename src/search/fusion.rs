use std::collections::HashMap;

/// Reciprocal-Rank Fusion: merge ranked lists without comparing raw scores.
/// `lists` pairs a weight with a best-first list of ids;
/// score(id) = Σ weight / (k + rank), rank 1-based. Deterministic output:
/// score desc, then id asc.
pub fn rrf(lists: &[(f32, Vec<i64>)], k: f32) -> Vec<(i64, f32)> {
    let mut scores: HashMap<i64, f32> = HashMap::new();
    for (weight, list) in lists {
        for (i, id) in list.iter().enumerate() {
            *scores.entry(*id).or_insert(0.0) += weight / (k + (i + 1) as f32);
        }
    }
    let mut out: Vec<(i64, f32)> = scores.into_iter().collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_strong_in_both_lists_wins() {
        let lists = vec![(1.0, vec![1, 2, 3]), (1.0, vec![2, 1, 4])];
        let fused = rrf(&lists, 60.0);
        let ids: Vec<i64> = fused.iter().map(|(id, _)| *id).collect();
        // 1 and 2 both appear high in both lists; 1 has ranks (1,2), 2 has (2,1) — tie.
        assert_eq!(ids[0], 1, "tie broken by id asc");
        assert_eq!(ids[1], 2);
        assert!(ids.contains(&3) && ids.contains(&4));
    }

    #[test]
    fn single_list_presence_still_visible() {
        let lists = vec![(1.0, vec![1]), (1.0, vec![2])];
        let fused = rrf(&lists, 60.0);
        assert_eq!(fused.len(), 2);
        assert!((fused[0].1 - fused[1].1).abs() < 1e-6);
    }

    #[test]
    fn weights_tilt_the_fusion() {
        let lists = vec![(2.0, vec![1]), (1.0, vec![2])];
        let fused = rrf(&lists, 60.0);
        assert_eq!(fused[0].0, 1);
        assert!(fused[0].1 > fused[1].1);
    }

    #[test]
    fn empty_input() {
        assert!(rrf(&[], 60.0).is_empty());
    }
}





