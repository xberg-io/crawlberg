# AI-RULEZ :: GENERATED FILE — DO NOT EDIT
# Content-Hash: blake3:24c219d28842d306ab1a4dea4970228462ed80a0965bb56f03101c2e82de1b1e
# Source-Hash: blake3:bc7c6d3c0ebe647009380997eaf32b3ff474f8bd1f2aa58f759c5ffa091f66cf
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
