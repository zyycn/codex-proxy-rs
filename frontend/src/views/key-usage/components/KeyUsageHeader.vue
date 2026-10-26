<script setup lang="ts">
import { ZButton, ZIconButton, ZPageHeader, ZSelect } from '@codex-proxy/ui'

import { KeyRound, LogOut, Moon, RefreshCw, Sun, Terminal } from '@lucide/vue'
import { shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import AppLanguageMenu from '@/components/AppLanguageMenu.vue'
import { useAuthStore } from '@/stores/modules/auth'
import { useThemeStore } from '@/stores/modules/theme'

defineProps<{ name?: string, prefix?: string, refreshing: boolean, configuring: boolean }>()

defineEmits<{ refresh: [], configure: [] }>()

const { t } = useI18n({ useScope: 'global' })

const period = defineModel<string>('period', { required: true })
const refreshInterval = defineModel<string>('refreshInterval', { required: true })
const auth = useAuthStore()
const theme = useThemeStore()
const router = useRouter()
const loggingOut = shallowRef(false)

async function logout() {
  loggingOut.value = true
  try {
    if (await auth.logout()) {
      await router.replace({
        name: 'login',
        state: { loginMode: 'key' },
      })
    }
  }
  finally {
    loggingOut.value = false
  }
}
</script>

<template>
  <ZPageHeader :title="t('usage.header.title')">
    <template #description>
      <span class="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1.5">
        <span class="min-w-0 truncate text-cp font-semibold text-cp-text" :title="name">
          {{ name || t('usage.header.currentKey') }}
        </span>
        <span v-if="prefix" class="inline-flex shrink-0 items-center gap-1.5 rounded-cp-sm bg-cp-fill-tertiary px-2 py-0.5 text-cp-text-secondary" :aria-label="t('usage.header.keyPrefix')">
          <KeyRound class="size-3" aria-hidden="true" />
          <code class="font-mono text-cp-xs leading-4 font-normal">{{ prefix }}…</code>
        </span>
      </span>
    </template>
    <template #actions>
      <div class="key-usage-actions flex max-w-[calc(100vw-32px)] flex-wrap items-center justify-end gap-2">
        <AppLanguageMenu size="default" variant="solid" />
        <ZButton :loading="configuring" @click="$emit('configure')">
          <Terminal class="size-4" />
          {{ t('keys.actions.configure') }}
        </ZButton>
        <ZSelect v-model="period" :aria-label="t('usage.header.statisticsTimeRange')" class="w-29" :options="[{ label: t('usage.header.today'), value: 'today' }, { label: t('usage.header.lastDays'), value: '7d' }, { label: t('usage.header.lastThirtyDays'), value: '30d' }]" />
        <ZSelect v-model="refreshInterval" :aria-label="t('usage.header.automaticRefreshFrequency')" class="w-30" :options="[{ label: t('usage.header.refreshEveryS'), value: '30' }, { label: t('usage.header.refreshInterval'), value: '60' }, { label: t('usage.header.pauseRefresh'), value: '0' }]" />
        <ZIconButton :aria-label="t('usage.header.refreshUsage')" variant="solid" :loading="refreshing" @click="$emit('refresh')">
          <template #loading>
            <RefreshCw class="size-4 animate-spin motion-reduce:animate-none" />
          </template>
          <RefreshCw class="size-4" />
        </ZIconButton>
        <ZIconButton :aria-label="t('usage.header.switchTheme')" variant="solid" @click="theme.toggleTheme($event)">
          <Sun v-if="theme.effectiveTheme === 'dark'" class="size-4" />
          <Moon v-else class="size-4" />
        </ZIconButton>
        <ZIconButton :aria-label="t('auth.signOut')" variant="solid" :disabled="loggingOut" @click="logout">
          <LogOut class="size-4" />
        </ZIconButton>
      </div>
    </template>
  </ZPageHeader>
</template>

<style scoped>
.key-usage-actions {
  --cp-icon-button-secondary-bg: var(--cp-button-secondary-bg);
  --cp-icon-button-secondary-hover-bg: var(--cp-button-secondary-hover-bg);
  --cp-icon-button-secondary-active-bg: var(--cp-button-secondary-active-bg);
  --cp-input-bg: var(--cp-button-secondary-bg);
  --cp-input-hover-bg: var(--cp-button-secondary-hover-bg);
  --cp-input-active-bg: var(--cp-button-secondary-active-bg);
}
</style>
