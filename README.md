# Aleph

A minimal fork of the [Zed](https://zed.dev) code editor.

> [!WARNING]
> This fork is not meant for use by anyone other than me. If you choose to use it, please do not make feature requests as they will not be added. This fork is about removing things and minimising the features present in the editor. You may report bugs however, and I will try my best to fix them, though I can make no promises. Also, this fork will remove all non-macOS-arm64 code and is not cross-platform, nor do I intend to make it so. In most cases, using the original Zed editor or maintaining your own fork will likely be a better decision than using this fork.

This fork removes:

- Support for all platforms other than macOS arm64
- AI features:
  - Providers: Anthropic, Bedrock, Cloud (whatever this means), Copilot, Deepseek, Google, Zeta
  - ACP and other agent features
  - Onboarding
  - Assistant tools and slash commands
  - Edit predictions
- Tests, benchmarks and examples (who needs them anyway)
- Custom fonts (IBM Plex Sans and Lilex)
- Themes (replace all with ZeroLimits theme)
- Sentry reporting
- Collab features:
  - Audio
  - Call
  - Channel
- CLIs:
  - Docs preprocessor
  - Schema generator
  - Theme importer
  - Extension CLI
- Debugger (imagine needing to debug code when you could just write it correctly the first time)
- Journal
- Jujutsu
