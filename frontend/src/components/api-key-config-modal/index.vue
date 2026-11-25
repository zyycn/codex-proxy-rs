<script setup lang="ts">
import { ZButton, ZDialog, ZIconButton, ZScrollbar, ZSegmented, ZSwitch } from '@codex-proxy/ui'

import { Apple, Copy, Monitor, Upload } from '@lucide/vue'

import { computed, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import ClientImportModal from '../ClientImportModal.vue'
import { buildCodexCcSwitchImportDeeplink, buildCodexConfig, CODEX_WEBSOCKET_ENABLED_BY_DEFAULT } from './client'

const props = defineProps<{
  title?: string
  apiKey: {
    name?: string
    key?: string
  } | null
  apiBaseUrl: string
}>()

const emit = defineEmits<{
  copy: [text: string]
  afterLeave: []
}>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ default: false })
const showImport = shallowRef(false)

const activePlatform = shallowRef('unix')
const websocketEnabled = shallowRef(CODEX_WEBSOCKET_ENABLED_BY_DEFAULT)

const platformOptions = [
  { label: 'macOS / Linux', value: 'unix', icon: Apple },
  { label: 'Windows', value: 'windows', icon: Monitor },
]

const keyValue = computed(() => props.apiKey?.key ?? '')
const configPath = computed(() =>
  activePlatform.value === 'windows'
    ? '%userprofile%\\.codex\\config.toml'
    : '~/.codex/config.toml',
)
const codexConfig = computed(() => buildCodexConfig({
  apiKey: keyValue.value,
  baseUrl: props.apiBaseUrl,
  websocketEnabled: websocketEnabled.value,
}))

function importToClient() {
  if (!keyValue.value)
    return
  window.location.href = buildCodexCcSwitchImportDeeplink({
    apiKey: keyValue.value,
    baseUrl: props.apiBaseUrl,
    providerName: props.apiKey?.name || 'codex-proxy-rs',
  })
}
</script>

<template>
  <ZDialog
    v-model="open"
    :title="title ?? t('keys.configuration.title')"
    :description="t('keys.configuration.description')"
    width="48rem"
    @closed="showImport = false; emit('afterLeave')"
  >
    <div class="flex flex-col gap-5">
      <div class="flex flex-wrap items-center justify-between gap-3">
        <ZSegmented v-model="activePlatform" :aria-label="t('keys.configuration.platform')" :options="platformOptions" />
        <ZSwitch
          v-model="websocketEnabled"
          :aria-label="t('keys.configuration.toggleWebSocket')"
          active-text="WS"
          inactive-text="WS"
          inline-prompt
          :width="56"
        />
      </div>

      <section class="overflow-hidden rounded-z-card bg-z-fill-quaternary shadow-z-tertiary">
        <div class="flex items-center justify-between gap-3 px-4 py-2.5">
          <span
            class="min-w-0 truncate font-mono text-z-sm font-emphasis text-z-text-secondary"
          >
            {{ configPath }}
          </span>
          <ZIconButton
            variant="solid"
            size="small"
            :aria-label="t('common.actions.copy')"
            @click="emit('copy', codexConfig)"
          >
            <Copy class="size-3.5" />
          </ZIconButton>
        </div>
        <ZScrollbar max-height="calc(100dvh - 21rem)">
          <div class="mx-3 mb-3 rounded-z bg-z-bg-container px-3.5 py-3 shadow-z-tertiary">
            <pre
              class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-z-sm leading-[1.65] font-emphasis text-z-text"
              v-text="codexConfig"
            />
          </div>
        </ZScrollbar>
      </section>
    </div>

    <template #footer>
      <ZButton :disabled="!keyValue" @click="showImport = true">
        <Upload class="size-4" />
        {{ t('keys.configuration.importTo') }}
      </ZButton>
      <ZButton type="primary" @click="open = false">
        {{ t('common.actions.close') }}
      </ZButton>
    </template>
  </ZDialog>
  <ClientImportModal v-if="open" v-model="showImport" @select="importToClient" />
</template>
