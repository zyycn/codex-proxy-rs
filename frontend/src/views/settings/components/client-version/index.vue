<script setup lang="ts">
import ClientVersionGuideModal from './components/ClientVersionGuideModal.vue'
import ClientVersionPolicyCard from './components/ClientVersionPolicyCard.vue'
import { useClientDownloads } from './composables/useClientDownloads'

defineOptions({ name: 'ClientVersionSettings' })

withDefaults(
  defineProps<{
    loading?: boolean
    desktopError?: string
    cliError?: string
  }>(),
  {
    loading: false,
    desktopError: '',
    cliError: '',
  },
)

const minCodexDesktopVersion = defineModel<string>('minCodexDesktopVersion', { required: true })
const minCodexCliVersion = defineModel<string>('minCodexCliVersion', { required: true })

const {
  open,
  loading: downloadsLoading,
  error,
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
    :desktop-error="desktopError"
    :cli-error="cliError"
    @help="showGuide"
  />

  <ClientVersionGuideModal
    v-model="open"
    :downloads="downloads"
    :loading="downloadsLoading"
    :error="error"
    @retry="load(false)"
    @refresh="load(true)"
  />
</template>
