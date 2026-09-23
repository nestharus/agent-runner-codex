#!/usr/bin/env python3
"""Pure request-shape controls for the headless native inventory prompt report."""
import unittest

import verify_codex_inventory as inventory


PROMPT = 'Synthetic configured instructions.'


def developer_item(text):
    return {'role': 'developer', 'content': [{'type': 'input_text', 'text': text}]}


class HeadlessPromptEvidenceTest(unittest.TestCase):
    def test_exact_developer_item_with_additional_native_text(self):
        body = {'input': [developer_item(PROMPT), developer_item('Native policy text')]}
        self.assertEqual(inventory.prompt_evidence(body, PROMPT), {
            'configured_prompt_match': 'exact_developer_item',
            'additional_developer_text_observed': True,
        })
        self.assertFalse(inventory.prompt_evidence(
            {'input': [developer_item(PROMPT)]}, PROMPT
        )['additional_developer_text_observed'])

    def test_top_level_fallback_with_additional_native_text(self):
        body = {'instructions': PROMPT + '\n', 'input': [developer_item('Native policy text')]}
        self.assertEqual(inventory.prompt_evidence(body, PROMPT), {
            'configured_prompt_match': 'top_level_instructions_after_trim',
            'additional_developer_text_observed': True,
        })

    def test_partial_developer_match_is_rejected(self):
        body = {'input': [developer_item(PROMPT + ' Native policy text')]}
        with self.assertRaisesRegex(AssertionError, 'System instruction source differs'):
            inventory.prompt_evidence(body, PROMPT)


if __name__ == '__main__':
    unittest.main()
