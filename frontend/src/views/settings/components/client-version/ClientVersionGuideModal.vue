<script setup lang="ts">
import type { CodexDesktopWindowsDownloads } from '@/api'

import { ZButton, ZDialog, ZSegmented } from '@codex-proxy/ui'
import { MonitorUp, PackageOpen, TerminalSquare } from '@lucide/vue'

import { shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'

import ClientCliInstallPanel from './ClientCliInstallPanel.vue'
import ClientDesktopInstallPanel from './ClientDesktopInstallPanel.vue'

defineOptions({ name: 'ClientVersionGuideModal' })

withDefaults(
  defineProps<{
    downloads?: CodexDesktopWindowsDownloads | null
    loading?: boolean
  }>(),
  {
    downloads: null,
    loading: false,
  },
)

const emit = defineEmits<{
  refresh: []
}>()

const { t } = useI18n({ useScope: 'global' })

type ClientSection = 'desktop' | 'cli'

const open = defineModel<boolean>({ default: false })
const activeSection = shallowRef<ClientSection>('desktop')
const sectionOptions = [
  { label: 'Codex Desktop', value: 'desktop', icon: MonitorUp },
  { label: 'Codex CLI', value: 'cli', icon: TerminalSquare },
]
</script>

<template>
  <ZDialog
    v-model="open"
    :title="t('settings.clientVersion.codexClientUpgrade')"
    :description="t('settings.clientVersion.guideDescription')"
    type="info"
    width="48rem"
  >
    <template #icon>
      <PackageOpen class="size-4.5 text-z-info" />
    </template>

    <div class="grid gap-4">
      <ZSegmented
        v-model="activeSection"
        :aria-label="t('settings.clientProfile.clientType')"
        :options="sectionOptions"
        size="large"
        class="w-full"
      />

      <ClientDesktopInstallPanel
        v-if="activeSection === 'desktop'"
        :downloads="downloads"
        :loading="loading"
        @refresh="emit('refresh')"
      />
      <ClientCliInstallPanel v-else />
    </div>

    <template #footer>
      <ZButton type="primary" @click="open = false">
        {{ t('common.actions.close') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
