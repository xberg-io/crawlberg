# AI-RULEZ :: GENERATED FILE — DO NOT EDIT
# Content-Hash: blake3:24c219d28842d306ab1a4dea4970228462ed80a0965bb56f03101c2e82de1b1e
# Source-Hash: blake3:270b514d208099f9eacb7f132591e13c2f8e7b1071c48d5e25bbf4de2dedb560
# Schema-Version: v1

"""Hermes adapter for crawlberg.

This generated no-op keeps the plugin loadable without inventing runtime behavior.
To add Hermes tools, hooks, commands, or other registrations:

1. Create .ai-rulez/hermes/index.py.
2. Implement register(ctx) in that user-owned source file.
3. Run ai-rulez generate --plugin.

Project-local Hermes plugins are trusted code. Enable them explicitly with
HERMES_ENABLE_PROJECT_PLUGINS=true and validate all external input.
"""


def register(ctx):
    """Register this plugin with Hermes Agent."""
    del ctx
