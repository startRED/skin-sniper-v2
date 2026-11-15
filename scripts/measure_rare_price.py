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
    ghosts = rows("SELECT listing_id, bought_at AS at, name, grp FROM ghosts "
                  "WHERE approved=1 AND kind='raro'")
    evaluations = rows("SELECT listing_id, at, name, rare_group AS grp FROM evaluations "
                       "WHERE reason='aprovada' AND rare_group IS NOT NULL")
    population = merge_population(ghosts, evaluations)
    for row in population:
        row['period'] = 'treino' if row['at'] < CUTOFF else 'pós-corte'
        # Nenhuma tabela lida conserva cobertura por faixa em cada releitura.
        row['label'] = classify(False, False, False, False)
        row['reason'] = 'releitura da faixa em 48h não demonstrável'
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
        'cases': population,
    }
    connection.rollback()
    return result


if __name__ == '__main__':
    with sqlite3.connect(DATABASE, uri=True) as database:
        print(json.dumps(measure(database), ensure_ascii=False, indent=2))
