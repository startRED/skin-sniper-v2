//! Comparação das leituras registradas, por faixa de seed e intervalo de espera.

pub(crate) struct Comparison {
    pub range: String,
    pub band: i64,
    pub policy: i64,
    pub control: i64,
    #[cfg(test)]
    pub policy_fresh: i64,
    #[cfg(test)]
    pub control_fresh: i64,
    #[cfg(test)]
    pub policy_new_count: i64,
    #[cfg(test)]
    pub control_new_count: i64,
    #[cfg(test)]
    pub policy_new: i64,
    #[cfg(test)]
    pub control_new: i64,
    pub policy_hits: i64,
    pub control_hits: i64,
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
        coalesce(sum(CASE WHEN controle=1 THEN new_listings END),0),
        sum(CASE WHEN controle=0 AND fresh>0 THEN 1 ELSE 0 END),
        sum(CASE WHEN controle=1 AND fresh>0 THEN 1 ELSE 0 END)
        FROM r GROUP BY faixa,banda ORDER BY faixa,banda",
    )?;
    let rows = stmt.query_map([since, until], |r| {
        Ok(Comparison {
            range: r.get(0)?,
            band: r.get(1)?,
            policy: r.get(2)?,
            control: r.get(3)?,
            #[cfg(test)]
            policy_fresh: r.get(4)?,
            #[cfg(test)]
            control_fresh: r.get(5)?,
            #[cfg(test)]
            policy_new_count: r.get(6)?,
            #[cfg(test)]
            control_new_count: r.get(7)?,
            #[cfg(test)]
            policy_new: r.get(8)?,
            #[cfg(test)]
            control_new: r.get(9)?,
            policy_hits: r.get(10)?,
            control_hits: r.get(11)?,
        })
    })?;
    rows.collect()
}
