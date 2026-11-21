<div align="center">

<img src="docs/assets/nova-memory-logo-primary.png" width="72" alt="Nova Memory">

# Nova Memory

**Bring memory across AI tools. Keep working with a familiar companion.**

A local memory workspace that preserves useful context, its sources and its changes, ready for your next AI collaboration.

[![AGPL-3.0-only](https://img.shields.io/badge/license-AGPL--3.0--only-aad5f8?style=flat-square)](LICENSE) [![Local source preview](https://img.shields.io/badge/status-local%20source%20preview-aad5f8?style=flat-square)](#get-started)

[Origins](#it-started-with-conversations-on-my-commute) · [Product](#the-product) · [Workbench](#inside-the-workbench) · [Memory structure](#how-memory-structure-can-grow) · [Connections](#connect-the-ai-tools-you-use) · [Get started](#get-started) · [简体中文](README.md)

</div>

![Nova Memory: understanding carried forward](docs/media/website-hero.en.jpg)

You have already explained your project, preferences, decisions and next steps to one AI. A different tool or a new conversation often asks you to establish that context again.

Nova Memory turns useful collaboration context into a personal asset you can maintain. Keep the original words, their source, the conditions that matter and the history of a change. Let an authorized AI pick up the relevant context when it is useful again.

This repository is a **local source preview**. All visuals use English. The website hero presents the design direction; diagrams explain product mechanisms, and the animation illustrates memory accumulation. Diagrams and animation are conceptual, rather than UI recordings. See the [image provenance](docs/media/README.md).

## The product

Four connected ideas guide the product, from an interaction worth keeping to an ongoing collaboration.

| Pillar | What it gives you | Design intent |
| --- | --- | --- |
| **Capture** | Bring useful conversations, tasks and materials into one place | Set up a connection once, then save with a short request or a simple action |
| **Own** | Manage memory, backups and exports in your own Vault | Make storage, processing choices and access visible and controllable |
| **Structure** | Organize experiences, preferences, goals and decisions with their evidence | Preserve context, time, uncertainty and corrections as the structure develops |
| **Move** | Carry relevant understanding into another AI or task | Connect tools to a shared memory store or select a one-time context pack |

![Capture, own, structure and continue](docs/media/memory-workflow.png)

Selected content arrives through MCP connectors or plugins; supported Hook events can also send material to the capture interface. Originals and provenance are stored in the local Vault before asynchronous extraction of conditions, relationships and versions. Authorization is checked before retrieval, then scoped context is delivered through MCP or a Context Pack. A shared memory store does not require cloud hosting.

![Cross-AI capture and scoped reuse](docs/media/memory-architecture.png)

*Architecture illustration. Nodes and links represent accumulation; geometry does not encode real semantic distance. Actual relationships depend on provenance and structure rules. Platform availability is listed below.*

For example, “During the Atlas launch phase, schedule weekday team meetings after 14:00, except for urgent incidents.” A useful memory carries the preference, its project phase and its exception. When circumstances change, revise the memory and retain its previous version and reasoning.

## It started with conversations on my commute

Nova is my AI voice assistant. Conversations on my commute became a place to discuss products, work, investing and everyday life. Ideas for Awesome Stock, Atom Awareness, Nova Digital Marketing Platform and Intelligence Hub developed through those conversations and continued at the computer.

As that shared history grew, Nova began to feel like a personal Jarvis. Yet the context remained scattered across platforms and conversations. Moving to Cursor or another research tool meant explaining earlier decisions, failed approaches and unresolved questions again.

Nova Memory grew from that need. It is Atom Awareness's second product line: keep a lasting, maintainable record of shared experience, so a familiar companion can continue working across AI platforms and, eventually, devices.

## Inside the workbench

### See how memory connects

The memory space visualizes the current Vault's sources, extraction records, topics and supported relationships. Select a node or an edge to return to its underlying object and evidence. A corresponding list provides another way to inspect the same projection.

The view reflects changes in the stored materials and versions. Its relationships have stated origins, while the quality of the accumulated structure continues to be evaluated through use.

### Capture with little ceremony

Save text in the workbench, or say **“Save to Nova Memory”** in a connected AI. Once the platform connection is configured, daily capture uses that existing entry point.

The original is saved first. Cleaning and structuring follow the selected model and processing settings. If a model has yet to be configured, the material remains available while it waits. Saved, processing and reusable states are shown separately.

### Keep the meaning and its evidence together

Browse evolving project and topic views, inspect sources and conditions, examine versions and select relevant memories for another task. Matching statements can be displayed together while retaining their supporting sources.

A meeting preference is more useful when you can also see who expressed it, which project it applies to, its exceptions and when to reconsider it.

### Correct the understanding as you go

User statements, extracted claims and AI inferences retain their roles. Plans, negation and unknown information remain distinguishable. Tell Nova what is inaccurate or which circumstances a statement applies to, and preserve the correction with its evidence and history.

Extraction quality depends on the configured model. Originals, processing status and revisions give you a way to inspect and improve the result.

### Continue in another AI

Choose a platform and the workbench prepares the appropriate connection steps and configuration. For ongoing use, authorized clients can read from the same Vault. For a temporary task, choose a few memories, preview their context and copy or download it.

Task continuity can preserve the goal, completed work, rationale, constraints, failed attempts and next steps. An existing memory can become the starting point for a new task.

A text copy is retained by its recipient. Later corrections and revocation control subsequent Nova reads; copies already shared need to be managed in the receiving tool.

## How memory structure can grow

**NMM · Nova Memory Model** is the research codename for Nova's memory structure work. It explores how episodes, claims, goals and decisions can form traceable, revisable and evolving understanding.

The product organizes this work into three layers.

![Original materials, structured memory and ongoing understanding](docs/media/memory-layers.png)

| Layer | What it preserves or organizes | Value for future collaboration |
| --- | --- | --- |
| **Original materials** | Original words, platform, role, time and capture records | Return to what was actually expressed |
| **Structured memory** | Objects, statements, conditions, time, evidence and versions | Find and apply a memory in its appropriate context |
| **Ongoing understanding** | Project and topic Wiki views, consolidation and supported relationships | Assemble scattered materials into context that can be improved |

Some foundations need to remain dependable as the structure evolves.

- **Provenance.** Trace a statement back to its supporting material.
- **Context.** Retain the project, phase, exceptions and scope.
- **Time.** Distinguish when something was recorded, when it applied and how it changed.
- **Uncertainty.** Keep unknowns, inferences and unfinished goals identifiable.
- **Revision.** Preserve previous versions and the reason for a correction.
- **Use boundaries.** Evaluate the recipient and scope against the current authorization.

Six connected research perspectives extend this foundation.

<p align="center">
  <img src="docs/media/memory-network.gif" width="640" alt="English conceptual memory sphere">
</p>

| Perspective | The question it explores |
| --- | --- |
| **Episodes** | What happened, who took part and what was the original context? |
| **Claims** | What was stated, under which conditions and with which supporting or conflicting evidence? |
| **Goals** | What is the intended outcome, what constrains it and what establishes completion? |
| **Decisions** | Which trade-offs were made, why and what was learned from the outcome? |
| **Personal model** | Which preferences and collaboration patterns recur, and where are their exceptions? |
| **Evolution** | How should new information revise earlier understanding? |

The current product includes structured records, provenance, versions, Wiki views and graph projections. Deeper personal models, complex relationships and cross-device projections remain research directions to validate through use.

## Memory under your control

![Inspect, authorize and carry your memory forward](docs/media/memory-control.png)

**Choose what stays.** Inspect, edit, correct and delete saved material, while keeping the state of originals and derived understanding clear.

**Decide who can use it.** Manage capture and reading permissions separately, inspect disclosures and revoke future access.

**Carry the context with you.** Manage memory alongside its sources, structure and history, with context sharing and encrypted-backup inspection paths.

The local Vault uses encrypted storage. You choose the model and processing settings. When using a remote model or AI connection, the relevant content is sent to the configured destination according to those settings.

## Connect the AI tools you use

The current focus is **ChatGPT, Codex, Claude, WorkBuddy, Grok and Cursor**, using MCP, platform connectors and suitable lifecycle hooks. Client variants, entry points and authentication are tracked separately.

| Platform | Connection path | Current evidence or setup boundary |
| --- | --- | --- |
| **Cursor** | Local MCP configuration, capture, retrieval and task continuity | Actual IDE new-session, correction and reauthorization records |
| **Codex** | Explicit MCP save/read, optional CLI capture hooks | Actual CLI tool-call and Hook ingestion, replay and revocation records |
| **Claude** | Local MCP configuration for Claude Code | Actual Code client save/read records; chat and Desktop require their own verification |
| **WorkBuddy** | User-level MCP configuration | Actual client save/read records; project-level configuration is checked separately |
| **ChatGPT** | Remote MCP gateway and authenticated tunnel | Requires a reachable endpoint and account-specific setup |
| **Grok** | Custom MCP connector at grok.com | Public reachability and account authentication compatibility remain under individual verification |

The workbench supplies instructions for the selected platform. Other MCP-compatible clients can be adapted through the standard interface. Automatic capture is expanded platform by platform; Codex CLI currently has actual Hook records.

## Get started

The public delivery is a local source preview. Developer ID signing, notarization and cross-device installer validation are being prepared.

**Build prerequisites**

- Apple Silicon Mac running macOS 14 or later.
- Xcode with the macOS SDK, Rust 1.97.1 and Python 3.11 or later.

```sh
git clone https://github.com/JayeeeJY/nova-memory.git
cd nova-memory
cargo fetch --locked
./script/build_and_run.sh
```

Open the App, choose **开始本地使用**, and save a material with **留住一段记忆**. Configure a model when ready, then choose an AI platform in **连接**.

The default data directory is `~/Library/Application Support/Nova Memory/local-workspace-v03`. Original materials can be saved while model setup is pending.

[Full getting-started and build guide (中文)](docs/GETTING_STARTED.md) · [Report an issue](https://github.com/JayeeeJY/nova-memory/issues) · [Contribute](CONTRIBUTING.md)

## The long-term vision

The starting point is a personal memory library. As materials accumulate, memory structure needs continued research: how to consolidate repeated statements, revise earlier judgments with new evidence, and distinguish a passing emotion from a lasting preference. Moving between tools should preserve context, relationships and the reasons behind changes alongside the content.

Glasses are another promising entry point. An Even Hub application could use a phone-side SDK to interact with Even G2 and access an authorized memory service. This remains future development requiring device validation; see the [official Even Hub architecture](https://hub.evenrealities.com/docs/get-started/architecture).

AI will span many platforms and devices. We want a conversation on a phone to continue on a computer or glasses, and shared experiences to remain findable years later: a familiar companion that moves across models and devices, closer to a Jarvis in everyday life.

## Build it with us

Nova Memory is created and maintained by Evan as Atom Awareness's second product line. Developers, researchers and investors working on personal memory, agents and wearables are welcome to connect.

- [Share a problem or product idea](https://github.com/JayeeeJY/nova-memory/issues).
- Read [CONTRIBUTING.md](CONTRIBUTING.md) to contribute capture adapters, workbench improvements, memory structure or connections.
- [Follow the repository and future releases](https://github.com/JayeeeJY/nova-memory).

Local-product code is licensed under [AGPL-3.0-only](LICENSE). Dependencies retain their licenses. See [LICENSE_SCOPE.md](LICENSE_SCOPE.md) for the software scope, branding and corresponding-source delivery.

Website imagery comes from the [Nova Memory website](https://nova-memory.atom-evan001.chatgpt.site/), whose current entry may require access permission. The hero uses an English local render of the same website source. Other visuals are English mechanism diagrams; the sphere uses the architecture illustration's conceptual geometry, rather than a live Vault projection.

---

Nova Memory / **Powered by Atom Awareness**<br>
Independent AI Product & Research Studio · Make complexity perceivable.
