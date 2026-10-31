//! Sınıflandırma metrikleri: ROC AUC (Mann–Whitney, eşitlikler ortalama sıra) ve log-loss.

/// ROC AUC. Pozitif ya da negatif örnek yoksa, uzunluklar uyuşmazsa ya da skor sonlu değilse `None`.
pub fn auc(scores: &[f64], labels: &[bool]) -> Option<f64> {
    if scores.len() != labels.len() || scores.iter().any(|s| !s.is_finite()) {
        return None;
    }
    let n_pos = labels.iter().filter(|y| **y).count();
    let n_neg = labels.len() - n_pos;
    if n_pos == 0 || n_neg == 0 {
        return None;
    }
    let mut idx: Vec<usize> = (0..scores.len()).collect();
    // Kararlı sıralama + toplam sıra → deterministik.
    idx.sort_by(|&a, &b| scores[a].total_cmp(&scores[b]));
    let mut rank_sum_pos = 0.0f64;
    let mut i = 0usize;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && scores[idx[j + 1]] == scores[idx[i]] {
            j += 1;
        }
        // i..=j eşit skorlar: ortalama sıra (1 tabanlı).
        let avg_rank = (i + j) as f64 / 2.0 + 1.0;
        for &k in &idx[i..=j] {
            if labels[k] {
                rank_sum_pos += avg_rank;
            }
        }
        i = j + 1;
    }
    let np = n_pos as f64;
    let nn = n_neg as f64;
    Some((rank_sum_pos - np * (np + 1.0) / 2.0) / (np * nn))
}

/// Ortalama ikili log-loss; olasılıklar [1e-15, 1−1e-15] aralığına kırpılır. Boş girdide `None`.
pub fn log_loss(probs: &[f64], labels: &[bool]) -> Option<f64> {
    if probs.is_empty() || probs.len() != labels.len() || probs.iter().any(|p| !p.is_finite()) {
        return None;
    }
    const EPS: f64 = 1e-15;
    let sum: f64 = probs
        .iter()
        .zip(labels)
        .map(|(p, y)| {
            let p = p.clamp(EPS, 1.0 - EPS);
            if *y { -p.ln() } else { -(1.0 - p).ln() }
        })
        .sum();
    Some(sum / probs.len() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auc_known_values() {
        assert_eq!(
            auc(&[0.1, 0.2, 0.8, 0.9], &[false, false, true, true]),
            Some(1.0)
        );
        assert_eq!(
            auc(&[0.9, 0.8, 0.2, 0.1], &[false, false, true, true]),
            Some(0.0)
        );
        // Hepsi eşit → 0.5.
        assert_eq!(auc(&[0.5; 4], &[false, true, false, true]), Some(0.5));
        // 1 pozitif (0.4) 2 negatif (0.3, 0.6) → 1/2.
        assert_eq!(auc(&[0.3, 0.4, 0.6], &[false, true, false]), Some(0.5));
        assert_eq!(auc(&[0.3, 0.4], &[true, true]), None);
    }

    #[test]
    fn log_loss_known_values() {
        let l = log_loss(&[0.5, 0.5], &[true, false]).unwrap();
        assert!((l - std::f64::consts::LN_2).abs() < 1e-15);
        assert!(log_loss(&[1.0], &[false]).unwrap() > 30.0);
        assert_eq!(log_loss(&[], &[]), None);
    }
}
