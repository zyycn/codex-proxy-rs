<script setup lang="ts">
import type { BuiltinProvider } from '@/providers/catalog'
import { Apple, MicrosoftWindows, Tux } from '@boxicons/vue'
import { ZButton, ZDialog, ZIconButton, ZScrollbar, ZSegmented, ZSwitch } from '@codex-proxy/ui'
import { Copy, Upload } from '@lucide/vue'
import { computed, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import ClientImportModal from '@/components/ClientImportModal.vue'
import ProviderSelect from '@/components/ProviderSelect.vue'
import { buildCcSwitchImportDeeplink, buildClientConfig, resolveServiceRootUrl } from './client'

const props = defineProps<{ title?: string, apiKey: { name?: string, key?: string } | null }>()
const emit = defineEmits<{ copy: [text: string], closed: [] }>()
const open = defineModel<boolean>({ default: false })
const { t } = useI18n({ useScope: 'global' })
const showImport = shallowRef(false)
const key = computed(() => props.apiKey?.key ?? '')
const provider = shallowRef<BuiltinProvider>('openai')
const platform = shallowRef('macos')
const ws = shallowRef(false)
const root = resolveServiceRootUrl()
const platformOptions = [
  { label: 'macOS', value: 'macos', icon: Apple },
  { label: 'Linux', value: 'linux', icon: Tux },
  { label: 'Windows', value: 'windows', icon: MicrosoftWindows },
]
const config = computed(() => buildClientConfig({
  provider: provider.value,
  root,
  apiKey: key.value,
  windows: platform.value === 'windows',
  websocketEnabled: ws.value,
}))

function importToClient() {
  if (!key.value)
    return
  window.location.href = buildCcSwitchImportDeeplink({
    provider: provider.value,
    apiKey: key.value,
    providerName: props.apiKey?.name || 'codex-proxy-rs',
  })
}
</script>

<template>
  <ZDialog v-model="open" destroy-on-close :title="title ?? t('keys.configuration.title')" width="46rem" @close="showImport = false" @closed="emit('closed')">
    <div class="grid min-w-0 gap-3">
      <div class="flex flex-wrap items-center justify-between gap-2">
        <ProviderSelect v-model="provider" :aria-label="t('keys.configuration.platform')" />
        <ZSegmented v-model="platform" :aria-label="t('keys.configuration.os')" :options="platformOptions" display="icon" class="w-30 [&_svg]:size-4.5" size="small" />
      </div>
      <section class="min-w-0 overflow-hidden rounded-z-card bg-z-fill-quaternary">
        <ZScrollbar max-height="min(58dvh, 34rem)">
          <div class="mx-3 mb-3 min-w-0">
            <div class="flex flex-wrap items-center gap-3 py-2">
              <code class="min-w-0 flex-1 break-all text-z-sm text-z-text-secondary">{{ config.title }}</code>
              <ZSwitch v-if="provider === 'openai'" v-model="ws" :aria-label="t('keys.configuration.toggleWebSocket')" active-text="WS" inactive-text="WS" inline-prompt :width="56" />
              <ZIconButton variant="solid" size="small" :disabled="!key" :aria-label="t('common.actions.copy')" @click="emit('copy', config.text)">
                <Copy class="size-3.5" />
              </ZIconButton>
            </div>
            <pre class="m-0 rounded-z bg-z-bg-container px-3.5 py-3 font-mono text-z-sm leading-relaxed wrap-anywhere whitespace-pre-wrap text-z-text" v-text="config.text" />
          </div>
        </ZScrollbar>
      </section>
    </div>
    <template #footer>
      <ZButton :disabled="!key" @click="showImport = true">
        <Upload class="size-4" /> {{ t('keys.configuration.importTo') }}
      </ZButton>
      <ZButton type="primary" @click="open = false">
        {{ t('common.actions.close') }}
      </ZButton>
    </template>
  </ZDialog>
  <ClientImportModal v-model="showImport" :provider="provider" @select="importToClient" />
</template>
