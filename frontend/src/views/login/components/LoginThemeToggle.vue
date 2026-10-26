<script setup lang="ts">
import { Moon, Sun } from '@lucide/vue'

import { storeToRefs } from 'pinia'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

import { useThemeStore } from '@/stores/modules/theme'

const { t } = useI18n({ useScope: 'global' })

const themeStore = useThemeStore()
const { effectiveTheme } = storeToRefs(themeStore)

const label = computed(() => effectiveTheme.value === 'dark'
  ? t('appearance.switchToLight')
  : t('appearance.switchToDark'))
</script>

<template>
  <button
    type="button"
    class="relative inline-grid h-9 w-20 shrink-0 cursor-pointer grid-cols-2 place-items-center rounded-[20px] border-0 bg-cp-bg-text-hover p-0 outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline"
    :aria-label="label"
    :title="label"
    @click="themeStore.toggleTheme($event)"
  >
    <Sun
      class="relative z-1"
      :class="effectiveTheme === 'light' ? 'text-cp-text' : 'text-cp-text-secondary'"
      :size="16"
    />
    <span
      class="absolute top-1 left-1.5 size-7 rounded-full bg-cp-bg-elevated transition-transform duration-200 motion-reduce:transition-none"
      :class="effectiveTheme === 'dark' ? 'translate-x-10' : undefined"
    />
    <Moon
      class="relative z-1"
      :class="effectiveTheme === 'dark' ? 'text-cp-text' : 'text-cp-text-secondary'"
      :size="16"
    />
  </button>
</template>
