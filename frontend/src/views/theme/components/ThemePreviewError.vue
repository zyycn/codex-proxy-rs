<script setup lang="ts">
import { useI18n } from 'vue-i18n'

defineProps<{ error?: Error }>()
const { t } = useI18n({ useScope: 'global' })
</script>

<template>
  <div class="grid min-h-96 place-content-center text-z-text-secondary" role="alert">
    {{ t('appearance.preview.loadFailed') }}
  </div>
</template>
