<script setup lang="ts">
import {
  ChartNoAxesColumn,
  FolderTree,
  KeyRound,
  LayoutDashboard,
  Network,
  Palette,
  Settings,
  Users,
} from '@lucide/vue'
import { storeToRefs } from 'pinia'
import { computed, onBeforeUnmount, onMounted, shallowRef } from 'vue'

import { getClientSystemVersion } from '@/api'
import { useAuthStore } from '@/stores/modules/auth'
import { useSystemUpdateStore } from '@/stores/modules/system-update'

import AppShell from './components/AppShell.vue'
import SystemUpdateModal from './components/SystemUpdateModal/index.vue'

const adminNavItems = [
  { label: '概览', icon: LayoutDashboard, path: '/' },
  { label: '账号管理', icon: Users, path: '/accounts' },
  { label: '代理管理', icon: Network, path: '/proxies' },
  { label: '分组管理', icon: FolderTree, path: '/account-groups' },
  { label: 'API 密钥', icon: KeyRound, path: '/api-keys' },
  { label: '使用统计', icon: ChartNoAxesColumn, path: '/usage' },
  { label: '主题设置', icon: Palette, path: '/theme' },
  { label: '系统设置', icon: Settings, path: '/settings' },
]

const keyNavItems = [
  { label: '概览', icon: LayoutDashboard, path: '/' },
  { label: '使用统计', icon: ChartNoAxesColumn, path: '/usage' },
  { label: '主题设置', icon: Palette, path: '/theme' },
]

const authStore = useAuthStore()
const { isAdmin } = storeToRefs(authStore)
const systemUpdateStore = useSystemUpdateStore()
const { hasUpdate, loadedOnce, version: adminVersion } = storeToRefs(systemUpdateStore)
const keyVersion = shallowRef('')
const systemUpdateOpen = shallowRef(false)
const systemUpdateOpening = shallowRef(false)
const navItems = computed(() => isAdmin.value ? adminNavItems : keyNavItems)
const version = computed(() => isAdmin.value ? adminVersion.value?.version ?? '' : keyVersion.value)
const gitSha = computed(() => isAdmin.value ? adminVersion.value?.gitSha ?? '' : '')

async function openSystemUpdate() {
  if (!isAdmin.value || systemUpdateOpen.value || systemUpdateOpening.value)
    return

  systemUpdateOpening.value = true
  try {
    if (!loadedOnce.value)
      await systemUpdateStore.loadSystem(false)
  }
  catch {
    // 弹窗负责呈现具体错误，此处仍允许管理员进入弹窗重试。
  }
  finally {
    systemUpdateOpening.value = false
    systemUpdateOpen.value = true
  }
}

onMounted(async () => {
  if (isAdmin.value) {
    await systemUpdateStore.loadVersion().catch(() => undefined)
    return
  }

  try {
    keyVersion.value = (await getClientSystemVersion({ silent: true })).version
  }
  catch {
    keyVersion.value = ''
  }
})

onBeforeUnmount(() => {
  if (isAdmin.value)
    systemUpdateStore.disconnectUpdateEvents()
})
</script>

<template>
  <AppShell
    :nav-items="navItems"
    :version="version"
    :git-sha="gitSha"
    :has-update="isAdmin && hasUpdate"
    :system-update-enabled="isAdmin"
    @open-system-update="openSystemUpdate"
  >
    <template v-if="isAdmin" #overlay>
      <SystemUpdateModal v-model="systemUpdateOpen" />
    </template>
  </AppShell>
</template>
