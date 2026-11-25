<script setup lang="ts">
import ClientVersionGuideModal from './ClientVersionGuideModal.vue'
import ClientVersionPolicyCard from './ClientVersionPolicyCard.vue'
import { useClientDownloads } from './useClientDownloads'

defineOptions({ name: 'ClientVersionSettings' })

withDefaults(
  defineProps<{
    loading?: boolean
  }>(),
  {
    loading: false,
  },
)

const minCodexDesktopVersion = defineModel<string>('minCodexDesktopVersion', { required: true })
const minCodexCliVersion = defineModel<string>('minCodexCliVersion', { required: true })

const {
  open,
  loading: downloadsLoading,
  downloads,
  load,
  showGuide,
} = useClientDownloads()
</script>

<template>
  <ClientVersionPolicyCard
    v-model:min-codex-desktop-version="minCodexDesktopVersion"
    v-model:min-codex-cli-version="minCodexCliVersion"
    :loading="loading"
    @help="showGuide"
  />

  <ClientVersionGuideModal
    v-model="open"
    :downloads="downloads"
    :loading="downloadsLoading"
    @refresh="load(true)"
  />
</template>
