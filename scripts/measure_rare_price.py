"""Medição D offline; só lê a fonte autorizada, nunca settings ou rede."""
import json
import sqlite3
from collections import Counter

DATABASE = 'file:E:/Documentos/ProjetosIA/SkinSniperV2/app/data/sniper.db?mode=ro'
CUTOFF = 1794558360


def merge_population(ghosts, evaluations):
    merged = {}
    for source, rows in [('ghosts', ghosts), ('evaluations', evaluations)]:
        for row in rows:
            key = row['listing_id']
            if key not in merged:
                merged[key] = dict(row, sources=[])
            merged[key]['at'] = min(merged[key]['at'], row['at'])
            if source not in merged[key]['sources']:
                merged[key]['sources'].append(source)
    for row in merged.values():
        row['sources'].sort()
    return sorted(merged.values(), key=lambda row: (row['at'], row['listing_id']))


def classify(coverage, vanished_cheapest, false_signal, find_complete):
    # Ordem literal D. find_complete exige continuidade, prazo e ausência de retorno.
    if not coverage or (vanished_cheapest and false_signal):
        return 'inconclusivo'
    if false_signal:
        return 'provável falso'
    if find_complete:
        return 'provável achado'
    return 'inconclusivo'


def measure(connection):
    connection.row_factory = sqlite3.Row
    connection.execute('PRAGMA query_only=ON')
    connection.execute('BEGIN')
    def rows(sql):
        return [dict(row) for row in connection.execute(sql)]
    ghosts = rows("SELECT listing_id, bought_at AS at, name, grp, price_cents, mid_price_cents FROM ghosts "
                  "WHERE approved=1 AND kind='raro'")
    evaluations = rows("SELECT listing_id, at, name, rare_group AS grp, price_cents, NULL AS mid_price_cents FROM evaluations "
                       "WHERE reason='aprovada' AND rare_group IS NOT NULL")
    population = merge_population(ghosts, evaluations)
    for row in population:
        row['period'] = 'treino' if row['at'] < CUTOFF else 'pós-corte'
        observation = connection.execute(
            'SELECT * FROM observations WHERE listing_id=?',
            (row['listing_id'],)).fetchone()
        observation = dict(observation) if observation else None
        below = [dict(other) for other in connection.execute(
            "SELECT listing_id, name, price_cents, first_seen, last_seen FROM observations "
            "WHERE grp=? AND listing_id!=? AND price_cents<? "
            "AND first_seen BETWEEN ? AND ? ORDER BY first_seen, listing_id",
            (row['grp'], row['listing_id'], row['mid_price_cents'],
             row['at'], row['at'] + 48 * 3600))]
        elapsed = observation['last_seen'] - row['at'] if observation else None
        persisted = bool(observation and elapsed >= 48 * 3600
                         and observation['price_cents'] <= row['price_cents'])
        vanished = bool(observation and observation['cheapest']
                        and observation['gone_at'] is not None
                        and row['at'] <= observation['gone_at'] <= row['at'] + 48 * 3600
                        and not observation['relisted'])
        false_signal = persisted or bool(below)
        row['signals'] = {
            'observation': observation,
            'persisted_seconds': elapsed, 'persisted_48h': persisted,
            'group_below': below,
            'forecast_available': row['mid_price_cents'] is not None,
            'vanished_initially_cheapest_48h': vanished,
            'conflict_candidate': vanished and false_signal,
            'false_signal': false_signal,
        }
        # Não há histórico de faixa nem de posição/preço: sinal não é rótulo comprovado.
        row['coverage'] = False
        row['label'] = classify(row['coverage'], vanished, false_signal, False)
        row['reason'] = 'releitura da faixa em 48h não demonstrável; sinais reportados separadamente'
        row['observation_present'] = connection.execute(
            'SELECT EXISTS(SELECT 1 FROM observations WHERE listing_id=?)',
            (row['listing_id'],)).fetchone()[0]
    result = {
        'cutoff_utc': '2026-11-13 08:26:00 UTC', 'cutoff_unix': CUTOFF,
        'population': 'aprovações históricas raras deduplicadas; não replay atual',
        'ghost_rows': len(ghosts), 'evaluation_rows': len(evaluations),
        'all_approved_evaluations': rows("SELECT COUNT(*) AS rows, COUNT(DISTINCT listing_id) AS distinct_ids FROM evaluations WHERE reason='aprovada'")[0],
        'evaluation_distinct': len({row['listing_id'] for row in evaluations}),
        'union_distinct': len(population),
        'period_counts': dict(Counter(row['period'] for row in population)),
        'label_counts': dict(Counter(row['label'] for row in population)),
        'reads': rows('SELECT COUNT(*) AS n, MIN(at) AS first_at, MAX(at) AS last_at FROM reads')[0],
        'guards': {key: 'não decidido' for key in
                   ['n>3', 'múltiplo enorme em skin de centavos', 'separar variantes']},
        'signal_counts_by_period': {
            period: {signal: sum(bool(row['signals'][signal]) for row in population
                                 if row['period'] == period)
                     for signal in ['persisted_48h', 'group_below',
                                    'vanished_initially_cheapest_48h',
                                    'conflict_candidate', 'false_signal']}
            for period in ['treino', 'pós-corte']},
        'cases': population,
    }
    connection.rollback()
    return result


if __name__ == '__main__':
    with sqlite3.connect(DATABASE, uri=True) as database:
        print(json.dumps(measure(database), ensure_ascii=False, indent=2))
