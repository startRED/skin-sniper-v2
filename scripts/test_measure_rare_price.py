import sqlite3
import unittest

from measure_rare_price import CUTOFF, merge_population, classify, measure


class MeasurementTests(unittest.TestCase):
    def test_union_deduplicates_and_keeps_first_approval(self):
        rows = merge_population(
            [{'listing_id': 'a', 'at': CUTOFF - 1}],
            [{'listing_id': 'a', 'at': CUTOFF + 1},
             {'listing_id': 'b', 'at': CUTOFF}])
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0]['at'], CUTOFF - 1)
        self.assertEqual(rows[0]['sources'], ['evaluations', 'ghosts'])
        self.assertEqual(rows[1]['at'], CUTOFF)

    def test_missing_coverage_wins_over_false_or_find(self):
        self.assertEqual(classify(False, True, True, True), 'inconclusivo')

    def test_conflicting_signals_win_over_false(self):
        self.assertEqual(classify(True, True, True, True), 'inconclusivo')

    def test_false_then_find_then_unknown(self):
        self.assertEqual(classify(True, False, True, True), 'provável falso')
        self.assertEqual(classify(True, False, False, True), 'provável achado')
        self.assertEqual(classify(True, False, False, False), 'inconclusivo')

    def test_measure_without_settings_and_excludes_common_approvals(self):
        with sqlite3.connect(':memory:') as connection:
            connection.executescript('''
                CREATE TABLE ghosts(listing_id, bought_at, name, grp, approved, kind);
                CREATE TABLE evaluations(listing_id, at, name, rare_group, reason);
                CREATE TABLE observations(listing_id);
                CREATE TABLE reads(at);
                INSERT INTO ghosts VALUES ('a', 1, 'skin', 'group', 1, 'raro');
                INSERT INTO evaluations VALUES ('a', 2, 'skin', 'group', 'aprovada');
                INSERT INTO evaluations VALUES ('a', 3, 'skin', 'group', 'aprovada');
                INSERT INTO evaluations VALUES ('b', 4, 'common', NULL, 'aprovada');
                INSERT INTO evaluations VALUES ('c', 1794558360, 'skin', 'group', 'aprovada');
            ''')
            result = measure(connection)
            self.assertEqual(result['union_distinct'], 2)
            self.assertEqual(result['evaluation_rows'], 3)
            self.assertEqual(result['all_approved_evaluations']['rows'], 4)
            self.assertEqual(result['period_counts'], {'treino': 1, 'pós-corte': 1})
            self.assertEqual(result['label_counts'], {'inconclusivo': 2})


if __name__ == '__main__':
    unittest.main()
