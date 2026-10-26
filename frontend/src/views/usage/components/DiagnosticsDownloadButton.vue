<script setup lang="ts">
import { ZButton } from '@codex-proxy/ui'
import { Download } from '@lucide/vue'
import { useI18n } from 'vue-i18n'
import { getDiagnosticsDownloadUrl } from '@/api'

const props = defineProps<{ requestId?: string | null, errorId?: string | null, active: boolean }>()

const { t } = useI18n({ useScope: 'global' })

function download() {
  const id = props.requestId || props.errorId
  if (!id || !props.active)
    return
  const target = props.requestId
    ? { requestId: props.requestId }
    : { errorId: id }
  window.open(getDiagnosticsDownloadUrl(target), '_blank', 'noopener,noreferrer')
}
</script>

<template>
  <ZButton type="primary" variant="plain" :aria-label="t('usage.diagnostics.downloadDiagnosticPackage')" :title="t('usage.diagnostics.downloadDiagnosticPackage')" :disabled="!active || (!requestId && !errorId)" @click="download">
    <template #icon>
      <Download :size="16" />
    </template>
    {{ t('usage.diagnostics.download') }}
  </ZButton>
</template>
