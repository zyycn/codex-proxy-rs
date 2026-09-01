<script setup lang="ts">
import { ExternalLink, TerminalSquare } from '@lucide/vue'

import ClientInstallCommandList from './ClientInstallCommandList.vue'

defineOptions({ name: 'ClientCliInstallPanel' })

const cliCommands = [
  {
    label: 'macOS / Linux · 官方安装器',
    command: 'curl -fsSL https://chatgpt.com/codex/install.sh | sh',
  },
  {
    label: 'Windows PowerShell · 官方安装器',
    command: 'irm https://chatgpt.com/codex/install.ps1 | iex',
  },
  {
    label: 'npm',
    command: 'npm install -g @openai/codex',
  },
  {
    label: 'Homebrew · 首次安装',
    command: 'brew install --cask codex',
  },
  {
    label: 'Homebrew · 升级',
    command: 'brew upgrade --cask codex',
  },
]
</script>

<template>
  <section class="grid gap-4" aria-labelledby="codex-cli-guide-title">
    <div class="flex items-start gap-3 px-1">
      <span class="inline-grid size-10 shrink-0 place-items-center rounded-cp bg-cp-fill-quaternary text-cp-text-secondary">
        <TerminalSquare class="size-4.5" />
      </span>
      <div class="min-w-0">
        <h3 id="codex-cli-guide-title" class="m-0 text-cp-lg leading-none font-heavy text-cp-text">
          安装或升级 Codex CLI
        </h3>
        <p class="mt-1.5 mb-0 text-cp-sm leading-[1.45] font-semibold text-cp-text-secondary">
          按当前安装来源执行一种命令；官方安装器会自动选择当前平台架构。
        </p>
      </div>
    </div>

    <div class="grid gap-3 rounded-cp-card bg-cp-fill-quaternary p-4">
      <p class="m-0 text-cp-sm font-heavy text-cp-text">
        选择安装来源
      </p>
      <ClientInstallCommandList :commands="cliCommands" />
    </div>

    <div class="flex flex-wrap items-center justify-between gap-3 px-1 text-cp-sm font-semibold text-cp-text-secondary">
      <span>重新执行对应命令即可升级到当前发布版本。</span>
      <a
        href="https://learn.chatgpt.com/docs/codex/cli"
        target="_blank"
        rel="noopener noreferrer"
        class="inline-flex shrink-0 items-center gap-1.5 font-bold text-cp-link transition-colors hover:text-cp-link-hover"
      >
        查看官方文档
        <ExternalLink class="size-3.5" />
      </a>
    </div>
  </section>
</template>
