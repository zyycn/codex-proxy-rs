<script setup lang="ts">
import { ZButton, ZSkeleton } from '@codex-proxy/ui'
import { useI18n } from 'vue-i18n'

withDefaults(defineProps<{
  loading: boolean
  error?: string
  variant?: 'request' | 'error'
}>(), { variant: 'request' })
defineEmits<{ retry: [] }>()
const { t } = useI18n({ useScope: 'global' })
</script>

<template>
  <div v-if="loading" class="grid min-w-0 gap-3" role="status" aria-live="polite" aria-busy="true">
    <span class="sr-only">{{ t('usage.request.loadingDetails') }}</span>
    <div v-if="variant === 'error'" class="grid gap-3 rounded-z-card bg-z-fill-quaternary p-4">
      <ZSkeleton shape="text" class="h-4 w-40" />
      <ZSkeleton shape="text" class="w-3/4" />
      <ZSkeleton shape="text" class="w-1/2" />
    </div>
    <div class="grid gap-3 lg:grid-cols-2">
      <div v-for="panel in variant === 'request' ? 4 : 2" :key="panel" class="grid gap-5 rounded-z-card bg-z-fill-quaternary p-4">
        <ZSkeleton shape="text" class="h-4 w-24" />
        <div v-for="row in variant === 'request' ? 3 : 4" :key="row" class="grid grid-cols-2 gap-4">
          <div v-for="column in 2" :key="column" class="grid gap-2">
            <ZSkeleton shape="text" class="h-3 w-1/3" />
            <ZSkeleton shape="text" class="h-4 w-3/4" />
          </div>
        </div>
      </div>
    </div>
    <div class="grid gap-5 rounded-z-card bg-z-fill-quaternary p-4">
      <ZSkeleton shape="text" class="h-4 w-24" />
      <ZSkeleton shape="text" class="h-4 w-2/3" />
      <div class="grid grid-cols-2 gap-4">
        <ZSkeleton shape="text" class="h-4 w-3/4" />
        <ZSkeleton shape="text" class="h-4 w-3/4" />
      </div>
    </div>
    <div v-if="variant === 'request'" class="grid gap-3 lg:grid-cols-2">
      <div v-for="panel in 2" :key="panel" class="grid gap-5 rounded-z-card bg-z-fill-quaternary p-4">
        <ZSkeleton shape="text" class="h-4 w-20" />
        <ZSkeleton class="h-24 w-full" />
      </div>
    </div>
  </div>
  <div v-else-if="error" class="grid min-h-64 place-content-center justify-items-center gap-4" role="alert">
    <p class="m-0 text-z-sm text-z-text-secondary">
      {{ error }}
    </p>
    <ZButton @click="$emit('retry')">
      {{ t('common.actions.refresh') }}
    </ZButton>
  </div>
</template>
