<script setup lang="ts">
import { LogOut, Moon, RefreshCw, Sun } from '@lucide/vue'
import { shallowRef } from 'vue'
import { useRouter } from 'vue-router'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import { useAuthStore } from '@/stores/modules/auth'
import { useThemeStore } from '@/stores/modules/theme'

defineProps<{ name?: string, prefix?: string, refreshing: boolean }>()
defineEmits<{ refresh: [] }>()
const period = defineModel<string>('period', { required: true })
const refreshInterval = defineModel<string>('refreshInterval', { required: true })
const auth = useAuthStore()
const theme = useThemeStore()
const router = useRouter()
const loggingOut = shallowRef(false)

async function logout() {
  loggingOut.value = true
  try {
    if (await auth.logout())
      await router.replace({ name: 'login', state: { loginMode: 'key' } })
  }
  finally {
    loggingOut.value = false
  }
}
</script>

<template>
  <BasePageHeader title="使用统计">
    <template #description>
      <span class="truncate leading-none">{{ name || '当前 API Key' }}</span>
      <span v-if="prefix" class="shrink-0 font-mono text-cp-sm leading-none text-cp-text-tertiary">{{ prefix }}…</span>
    </template>
    <template #actions>
      <div class="flex max-w-[calc(100vw-32px)] items-center justify-end gap-2">
        <BaseSelect v-model="period" aria-label="统计时间范围" class="w-29" :options="[{ label: '今天', value: 'today' }, { label: '近 7 天', value: '7d' }, { label: '近 30 天', value: '30d' }]" />
        <BaseSelect v-model="refreshInterval" aria-label="自动刷新频率" class="w-30" :options="[{ label: '30 秒刷新', value: '30' }, { label: '60 秒刷新', value: '60' }, { label: '暂停刷新', value: '0' }]" />
        <BaseIconButton label="刷新用量" variant="secondary" :loading="refreshing" @click="$emit('refresh')">
          <RefreshCw class="size-4" />
        </BaseIconButton>
        <BaseIconButton label="切换主题" @click="theme.toggleTheme($event)">
          <Sun v-if="theme.effectiveTheme === 'dark'" class="size-4" />
          <Moon v-else class="size-4" />
        </BaseIconButton>
        <BaseIconButton label="退出登录" :loading="loggingOut" @click="logout">
          <LogOut class="size-4" />
        </BaseIconButton>
      </div>
    </template>
  </BasePageHeader>
</template>
