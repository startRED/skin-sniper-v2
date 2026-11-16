//! Comparação das leituras registradas, por faixa de seed e intervalo de espera.

#[derive(Clone)]
pub(crate) struct Comparison {
    #[cfg(test)]
    pub range: String,
    pub band: i64,
    pub policy: i64,
    pub control: i64,
    pub policy_fresh: i64,
    pub control_fresh: i64,
    #[cfg(test)]
    pub policy_new_count: i64,
    #[cfg(test)]
    pub control_new_count: i64,
    #[cfg(test)]
    pub policy_new: i64,
    #[cfg(test)]
    pub control_new: i64,
}

impl Comparison {
    pub fn rare_rates(&self) -> Option<(f64, f64, bool)> {
        if self.policy < 30 || self.control < 30 {
            return None;
        }
        Some((
            self.policy_fresh as f64 / self.policy as f64,
            self.control_fresh as f64 / self.control as f64,
            i128::from(self.policy_fresh) * i128::from(self.control)
                >= i128::from(self.control_fresh) * i128::from(self.policy),
        ))
    }
}

/// Soma as faixas de seed na mesma banda antes de calcular as médias.
pub(crate) fn by_wait(rows: &[Comparison]) -> Vec<Comparison> {
    let mut totals: Vec<Comparison> = Vec::new();
    for row in rows {
        if let Some(total) = totals.iter_mut().find(|total| total.band == row.band) {
            total.policy += row.policy;
            total.control += row.control;
            total.policy_fresh += row.policy_fresh;
            total.control_fresh += row.control_fresh;
            #[cfg(test)]
            {
                total.policy_new_count += row.policy_new_count;
                total.control_new_count += row.control_new_count;
                total.policy_new += row.policy_new;
                total.control_new += row.control_new;
            }
        } else {
            totals.push(row.clone());
        }
    }
    totals.sort_by_key(|row| row.band);
    totals
}

pub(crate) fn compare(
    c: &rusqlite::Connection,
    since: i64,
    until: i64,
) -> rusqlite::Result<Vec<Comparison>> {
    let mut stmt = c.prepare(
        "WITH r AS (
        SELECT choice_range AS faixa, choice_control AS controle, fresh, new_listings,
        CASE WHEN choice_gap<600 THEN 0 WHEN choice_gap<3600 THEN 1
        WHEN choice_gap<21600 THEN 2 ELSE 3 END AS banda
        FROM reads WHERE choice_range IS NOT NULL AND choice_gap>=0
        AND choice_control IN (0,1) AND outcome='ok' AND at BETWEEN ?1 AND ?2)
        SELECT faixa,banda,
        sum(CASE WHEN controle=0 THEN 1 ELSE 0 END),
        sum(CASE WHEN controle=1 THEN 1 ELSE 0 END),
        sum(CASE WHEN controle=0 THEN fresh ELSE 0 END),
        sum(CASE WHEN controle=1 THEN fresh ELSE 0 END),
        count(CASE WHEN controle=0 THEN new_listings END),
        count(CASE WHEN controle=1 THEN new_listings END),
        coalesce(sum(CASE WHEN controle=0 THEN new_listings END),0),
        coalesce(sum(CASE WHEN controle=1 THEN new_listings END),0)
        FROM r GROUP BY faixa,banda ORDER BY faixa,banda",
    )?;
    let rows = stmt.query_map([since, until], |r| {
        Ok(Comparison {
            #[cfg(test)]
            range: r.get(0)?,
            band: r.get(1)?,
            policy: r.get(2)?,
            control: r.get(3)?,
            policy_fresh: r.get(4)?,
            control_fresh: r.get(5)?,
            #[cfg(test)]
            policy_new_count: r.get(6)?,
            #[cfg(test)]
            control_new_count: r.get(7)?,
            #[cfg(test)]
            policy_new: r.get(8)?,
            #[cfg(test)]
            control_new: r.get(9)?,
        })
    })?;
    rows.collect()
}
