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
                CREATE TABLE ghosts(listing_id, bought_at, name, grp, approved, kind, price_cents, mid_price_cents);
                CREATE TABLE evaluations(listing_id, at, name, rare_group, reason, price_cents);
                CREATE TABLE observations(listing_id, name, grp, price_cents, first_seen, last_seen, cheapest, gone_at, relisted);
                CREATE TABLE reads(at);
                INSERT INTO ghosts VALUES ('a', 1, 'skin', 'group', 1, 'raro', 100, 200);
                INSERT INTO evaluations VALUES ('a', 2, 'skin', 'group', 'aprovada', 100);
                INSERT INTO evaluations VALUES ('a', 3, 'skin', 'group', 'aprovada', 100);
                INSERT INTO evaluations VALUES ('b', 4, 'common', NULL, 'aprovada', 100);
                INSERT INTO evaluations VALUES ('c', 1794558360, 'skin', 'group', 'aprovada', 100);
            ''')
            connection.executescript("""
                INSERT INTO observations VALUES ('a','skin','group',100,1,172802,1,172803,0);
                INSERT INTO observations VALUES ('other','skin','group',150,10,20,0,NULL,0);
                INSERT INTO observations VALUES ('equal','skin','group',200,10,20,0,NULL,0);
                INSERT INTO observations VALUES ('late','skin','group',1,172802,172802,0,NULL,0);
                INSERT INTO observations VALUES ('wrong','skin','else',1,10,20,0,NULL,0);
                INSERT INTO observations VALUES ('variant','skin variant','group',120,11,20,0,NULL,0);
            """)
            result = measure(connection)
            case = result['cases'][0]
            self.assertTrue(case['signals']['persisted_48h'])
            self.assertEqual(case['signals']['persisted_seconds'], 172801)
            self.assertEqual([r['listing_id'] for r in case['signals']['group_below']], ['other', 'variant'])
            self.assertFalse(case['signals']['vanished_initially_cheapest_48h'])
            self.assertFalse(case['signals']['conflict_candidate'])
            self.assertTrue(case['signals']['false_signal'])
            self.assertFalse(case['coverage'])
            self.assertEqual(case['label'], 'inconclusivo')
            self.assertFalse(result['cases'][1]['signals']['persisted_48h'])
            self.assertFalse(result['cases'][1]['signals']['forecast_available'])
            self.assertEqual(result['cases'][1]['signals']['group_below'], [])
            self.assertEqual(result['union_distinct'], 2)
            self.assertEqual(result['evaluation_rows'], 3)
            self.assertEqual(result['all_approved_evaluations']['rows'], 4)
            self.assertEqual(result['period_counts'], {'treino': 1, 'pós-corte': 1})
            self.assertEqual(result['label_counts'], {'inconclusivo': 2})
            connection.execute("PRAGMA query_only=OFF")
            connection.execute("UPDATE observations SET last_seen=20, gone_at=100 WHERE listing_id='a'")
            connection.commit()
            conflict = measure(connection)['cases'][0]['signals']
            self.assertTrue(conflict['vanished_initially_cheapest_48h'])
            self.assertTrue(conflict['conflict_candidate'])
            self.assertFalse(conflict['persisted_48h'])
            connection.execute("PRAGMA query_only=OFF")
            connection.execute("UPDATE observations SET relisted=1 WHERE listing_id='a'")
            connection.commit()
            self.assertFalse(measure(connection)['cases'][0]['signals']['conflict_candidate'])



if __name__ == '__main__':
    unittest.main()
