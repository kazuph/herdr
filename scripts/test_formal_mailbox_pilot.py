import unittest

from scripts import formal_mailbox_pilot_drift_check


class FormalMailboxPilotDriftCheckTests(unittest.TestCase):
    def test_formal_mailbox_anchors_are_current(self):
        self.assertEqual(formal_mailbox_pilot_drift_check.main(), 0)
