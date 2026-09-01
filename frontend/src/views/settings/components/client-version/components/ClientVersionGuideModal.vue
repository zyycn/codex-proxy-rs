<script setup lang="ts">
import type { CodexDesktopWindowsDownloads } from '@/api'
import { MonitorUp, PackageOpen, TerminalSquare } from '@lucide/vue'
import { shallowRef } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'

import ClientCliInstallPanel from './ClientCliInstallPanel.vue'
import ClientDesktopInstallPanel from './ClientDesktopInstallPanel.vue'

defineOptions({ name: 'ClientVersionGuideModal' })

withDefaults(
  defineProps<{
    downloads?: CodexDesktopWindowsDownloads | null
    loading?: boolean
    error?: string
  }>(),
  {
    downloads: null,
    loading: false,
    error: '',
  },
)

const emit = defineEmits<{
  refresh: []
  retry: []
}>()

type ClientSection = 'desktop' | 'cli'

const open = defineModel<boolean>({ default: false })
const activeSection = shallowRef<ClientSection>('desktop')
const sectionOptions = [
  { label: 'Codex Desktop', value: 'desktop', icon: MonitorUp },
  { label: 'Codex CLI', value: 'cli', icon: TerminalSquare },
]
</script>

<template>
  <BaseModal
    v-model="open"
    title="Codex 客户端升级"
    description="先选择客户端，再查看对应的安装与升级方式"
    tone="info"
    size="lg"
  >
    <template #icon>
      <PackageOpen class="size-4.5 text-cp-info" />
    </template>

    <div class="grid gap-4">
      <BaseSegmented
        v-model="activeSection"
        label="客户端类型"
        :options="sectionOptions"
        size="lg"
        class="w-full"
      />

      <ClientDesktopInstallPanel
        v-if="activeSection === 'desktop'"
        :downloads="downloads"
        :loading="loading"
        :error="error"
        @retry="emit('retry')"
        @refresh="emit('refresh')"
      />
      <ClientCliInstallPanel v-else />
    </div>

    <template #footer>
      <BaseButton variant="primary" @click="open = false">
        关闭
      </BaseButton>
    </template>
  </BaseModal>
</template>
