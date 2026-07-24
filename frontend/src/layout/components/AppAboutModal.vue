<script setup lang="ts">
import { Github } from '@boxicons/vue'
import { ExternalLink } from '@lucide/vue'
import { storeToRefs } from 'pinia'
import { computed } from 'vue'

import BaseModal from '@/components/base/BaseModal.vue'
import { useSystemUpdateStore } from '@/stores/modules/system-update'

const open = defineModel<boolean>({ default: false })

const { version } = storeToRefs(useSystemUpdateStore())

const author = 'Zyy'
const githubUrl = 'https://github.com/zyycn/codex-proxy-rs'

function normalizeBuildValue(value: string | undefined) {
  const normalized = value?.trim()

  if (!normalized) {
    return ''
  }

  return ['unknown', 'null'].includes(normalized.toLowerCase()) ? '' : normalized
}

const versionDisplay = computed(() => {
  const versionText = normalizeBuildValue(version.value?.version).replace(/^v/i, '')
  return versionText ? `v${versionText}` : ''
})
const gitShaDisplay = computed(() => {
  const gitSha = normalizeBuildValue(version.value?.gitSha)
  return gitSha ? gitSha.slice(0, 8) : ''
})

const versionLine = computed(() => {
  const parts = [versionDisplay.value, gitShaDisplay.value].filter(Boolean)
  return parts.length ? `版本 ${parts.join(' · ')}` : '版本信息不可用'
})

const linkItems = [
  {
    label: 'GitHub',
    value: 'codex-proxy-rs',
    href: githubUrl,
    icon: Github,
  },
]
</script>

<template>
  <BaseModal v-model="open" title="关于" width="420px" hide-footer>
    <div class="grid gap-5">
      <section class="flex min-w-0 items-center gap-3">
        <span
          class="inline-flex size-9 shrink-0 items-center justify-center rounded-(--cp-icon-button-radius) bg-(--cp-bg-subtle) font-mono text-[15px] leading-none font-[820] text-(--cp-text-primary)"
        >
          Z
        </span>
        <div class="min-w-0">
          <p class="m-0 truncate text-[15px] leading-none font-[780] text-(--cp-text-primary)">
            {{ author }}
          </p>
          <p class="mt-1.5 mb-0 text-[12px] leading-none font-[650] text-(--cp-text-secondary)">
            Built by Zyy · Codex Proxy RS
          </p>
        </div>
      </section>

      <section class="grid gap-1">
        <div
          v-for="item in linkItems"
          :key="item.label"
          class="group flex min-w-0 items-center justify-between gap-3 rounded-(--cp-input-radius-base) px-1 py-2.5 transition-colors hover:bg-(--cp-bg-subtle)"
        >
          <div class="flex min-w-0 items-center gap-3">
            <span
              class="inline-flex size-8 shrink-0 items-center justify-center rounded-(--cp-icon-button-radius) bg-(--cp-bg-muted) text-(--cp-text-primary)"
            >
              <component :is="item.icon" class="size-5" />
            </span>
            <div class="min-w-0">
              <p class="m-0 text-[11px] leading-none font-[760] text-(--cp-text-muted)">
                {{ item.label }}
              </p>
              <p
                class="mt-2 mb-0 truncate font-mono text-[12px] leading-none font-[720] text-(--cp-text-primary)"
                :title="item.value"
              >
                {{ item.value }}
              </p>
            </div>
          </div>

          <a
            :href="item.href"
            target="_blank"
            rel="noreferrer"
            class="inline-flex size-7 shrink-0 items-center justify-center rounded-(--cp-icon-button-radius) text-(--cp-text-muted) transition-colors hover:bg-(--cp-bg-muted) hover:text-(--cp-info)"
            :aria-label="`打开 ${item.label}`"
          >
            <ExternalLink class="size-3.5" />
          </a>
        </div>
      </section>

      <p class="m-0 font-mono text-[11px] leading-none font-[650] text-(--cp-text-muted)">
        {{ versionLine }}
      </p>
    </div>
  </BaseModal>
</template>
