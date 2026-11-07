<script setup lang="ts">
import type { SidebarItem } from '@codex-proxy/ui'
import type { RouteLocationRaw } from 'vue-router'
import type { PluginManagementView } from '@/api'
import { ZIconButton, ZMotionIcon, ZSidebar } from '@codex-proxy/ui'
import {
  ArrowUpCircle,
  Blocks,
  BringToFront,
  ChartNoAxesColumn,
  FolderTree,
  KeyRound,
  LayoutDashboard,
  LogOut,
  Moon,
  Network,
  Palette,
  PanelLeftClose,
  PanelLeftOpen,
  Puzzle,
  Settings,
  Sun,
  Users,
} from '@lucide/vue'
import { storeToRefs } from 'pinia'
import { computed, onMounted, useTemplateRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { isNavigationFailure, NavigationFailureType, useRoute, useRouter } from 'vue-router'
import AppBrandMark from '@/components/AppBrandMark.vue'
import AppLanguageMenu from '@/components/AppLanguageMenu.vue'
import { useAuthStore } from '@/stores/modules/auth'
import { usePluginViewsStore } from '@/stores/modules/plugin-views'
import { useSystemUpdateStore } from '@/stores/modules/system-update'
import { useThemeStore } from '@/stores/modules/theme'
import { pluginPageLocation, shortPluginInstanceId } from '@/utils/plugin'

const props = withDefaults(defineProps<{
  collapse?: boolean
  mobile?: boolean
}>(), {
  collapse: false,
  mobile: false,
})
const emit = defineEmits<{
  close: []
  navigate: []
  openSystemUpdate: []
  toggle: []
}>()

const { t } = useI18n({ useScope: 'global' })
const sidebar = useTemplateRef<InstanceType<typeof ZSidebar>>('sidebar')
const route = useRoute()
const router = useRouter()
const authStore = useAuthStore()
const pluginViewsStore = usePluginViewsStore()
const systemUpdateStore = useSystemUpdateStore()
const themeStore = useThemeStore()
const { pageViews: pluginPageViews } = storeToRefs(pluginViewsStore)
const { version, hasUpdate } = storeToRefs(systemUpdateStore)
const { effectiveTheme } = storeToRefs(themeStore)
const { toggleTheme } = themeStore
const isCollapsed = computed(() => !props.mobile && props.collapse)

const pluginViewNameCounts = computed(() => pluginPageViews.value.reduce((counts, view) => {
  const title = view.pages[0]?.title ?? view.name
  counts.set(title, (counts.get(title) ?? 0) + 1)
  return counts
}, new Map<string, number>()))

const navItems = computed<SidebarItem[]>(() => [
  {
    index: '/',
    label: t('navigation.overview'),
    icon: LayoutDashboard,
  },
  {
    index: '/accounts',
    label: t('navigation.accounts'),
    icon: Users,
  },
  {
    index: '/proxies',
    label: t('navigation.proxies'),
    icon: Network,
  },
  {
    index: '/groups',
    label: t('navigation.groups'),
    icon: FolderTree,
  },
  {
    index: '/keys',
    label: t('navigation.apiKeys'),
    icon: KeyRound,
  },
  {
    index: '/usage',
    label: t('navigation.usage'),
    icon: ChartNoAxesColumn,
  },
  {
    index: 'plugins',
    label: t('navigation.plugins'),
    icon: Puzzle,
    children: [
      {
        index: '/plugins',
        label: t('navigation.pluginManagement'),
        icon: BringToFront,
      },
      ...pluginPageViews.value.map(view => ({
        index: `plugin:${view.target.instanceId}`,
        label: pluginViewLabel(view),
        icon: Blocks,
      })),
    ],
  },
  {
    index: '/theme',
    label: t('navigation.appearance'),
    icon: Palette,
  },
  {
    index: '/settings',
    label: t('navigation.settings'),
    icon: Settings,
  },
])
const activeIndex = computed(() => {
  if (route.path.startsWith('/plugins')) {
    if (route.name === 'plugins')
      return '/plugins'
    const view = pluginPageViews.value.find(isPluginViewActive)
    return view
      ? `plugin:${view.target.instanceId}`
      : 'plugins'
  }
  return navItems.value.find(item => item.index !== '/' && route.path.startsWith(item.index))?.index ?? '/'
})

function isPluginViewActive(view: PluginManagementView) {
  return route.name === 'plugin-page' && route.params.instanceId === view.target.instanceId
}

function pluginViewLabel(view: PluginManagementView) {
  const title = view.pages[0]?.title ?? view.name
  if (pluginViewNameCounts.value.get(title) === 1)
    return title
  const duplicateName = pluginPageViews.value.some(item => item !== view && item.name === view.name && item.pages[0]?.title === title)
  return `${title} · ${duplicateName
    ? shortPluginInstanceId(view.target.instanceId)
    : view.name}`
}

async function handleSelect(index: string) {
  // 路由守卫可能取消跳转，菜单保持当前页，直到实际路由改变
  sidebar.value?.updateActiveIndex(activeIndex.value)
  let target: RouteLocationRaw = index === 'plugins'
    ? '/plugins'
    : index
  if (index.startsWith('plugin:')) {
    const view = pluginPageViews.value.find(item => `plugin:${item.target.instanceId}` === index)
    const page = view?.pages[0]
    if (!view || !page)
      return
    target = pluginPageLocation(view, page)
  }
  const failure = await router.push(target)
  if (!failure || isNavigationFailure(failure, NavigationFailureType.duplicated))
    emit('navigate')
}

function openSystemUpdate() {
  emit('openSystemUpdate')
}

async function handleLogout() {
  if (!await authStore.logout())
    return
  await router.push('/login')
  emit('navigate')
}

const themeToggleLabel = computed(() => effectiveTheme.value === 'dark'
  ? t('appearance.switchToLight')
  : t('appearance.switchToDark'))
const versionText = computed(() => version.value?.version.trim() ?? '')
const hasVersionLabel = computed(() => versionText.value.length > 0)
const versionLabel = computed(() => `v${versionText.value}`)
const updateButtonLabel = computed(() => hasUpdate.value
  ? t('navigation.update.available')
  : t('navigation.update.open'))

onMounted(() => {
  void pluginViewsStore.ensureLoaded().catch(() => undefined)
})
</script>

<template>
  <ZSidebar
    ref="sidebar"
    class="z-20"
    :class="mobile ? 'flex' : 'hidden min-[961px]:flex'"
    :items="navItems"
    :default-active="activeIndex"
    :collapse="isCollapsed"
    :navigation-label="t('navigation.mainLabel')"
    @select="handleSelect"
  >
    <template #brand-icon>
      <ZMotionIcon
        variant="brand"
        class="inline-flex size-11 items-center justify-center relative -top-0.5 rounded-z"
      >
        <AppBrandMark class="block size-11 select-none" />
      </ZMotionIcon>
    </template>
    <template #brand>
      <strong class="text-base leading-[1.1] font-heavy text-z-text"> Codex Proxy </strong>
      <span class="mt-1.5 flex h-4.5 min-w-0 items-center gap-2">
        <span class="shrink-0 text-xs leading-none font-emphasis text-z-text-secondary"> Rust build </span>
        <button
          v-if="hasVersionLabel"
          type="button"
          class="inline-flex h-4.5 min-w-0 cursor-pointer items-center gap-1 rounded-z-sm border-0 px-1.5 font-mono text-[10px] leading-none font-bold transition-colors outline-none focus-visible:ring-2 focus-visible:ring-z-control-outline focus-visible:ring-offset-2 focus-visible:ring-offset-z-bg-container"
          :class="[
            hasUpdate
              ? 'bg-z-success-container text-z-success-on-container hover:bg-z-success-container-hover'
              : 'bg-z-fill-quaternary text-z-text-quaternary hover:bg-z-fill-tertiary hover:text-z-text-secondary',
          ]"
          :aria-label="updateButtonLabel"
          @click="openSystemUpdate"
        >
          <span>{{ versionLabel }}</span>
          <ArrowUpCircle v-if="hasUpdate" class="size-3 shrink-0 text-z-success" />
        </button>
      </span>
    </template>
    <template #footer>
      <div
        class="bg-z-fill-quaternary"
        :class="isCollapsed ? 'grid gap-1 rounded-z p-1' : 'grid min-h-11 grid-cols-[4.25rem_auto] items-center justify-between rounded-z-lg px-2'"
      >
        <span
          v-if="!isCollapsed"
          class="inline-flex h-7 items-center justify-self-start gap-1.5 whitespace-nowrap rounded-lg bg-z-success-container px-2 text-xs leading-none font-emphasis text-z-success-on-container"
        >
          <i class="size-1.5 rounded-full bg-z-success" />
          {{ t('navigation.online') }}
        </span>

        <div :class="isCollapsed ? 'grid justify-items-center gap-1' : 'flex shrink-0 items-center gap-1 [&_button]:max-w-8'">
          <ZIconButton
            v-if="isCollapsed && hasUpdate"
            type="success" variant="plain"
            size="default"
            :aria-label="updateButtonLabel"
            @click="openSystemUpdate"
          >
            <ArrowUpCircle :size="19" />
          </ZIconButton>

          <ZIconButton
            :size="isCollapsed ? 'default' : 'small'"
            :aria-label="t('auth.signOut')"
            type="danger"
            :disabled="authStore.loading || !authStore.isAuthenticated"
            @click="handleLogout"
          >
            <LogOut :size="isCollapsed ? 19 : 18" />
          </ZIconButton>

          <ZIconButton
            :size="isCollapsed ? 'default' : 'small'"
            :aria-label="themeToggleLabel"
            @click="toggleTheme($event)"
          >
            <Sun v-if="effectiveTheme === 'dark'" :size="isCollapsed ? 19 : 18" />
            <Moon v-else :size="isCollapsed ? 19 : 18" />
          </ZIconButton>

          <AppLanguageMenu :size="isCollapsed ? 'default' : 'small'" />

          <ZIconButton v-if="mobile" size="small" :aria-label="t('navigation.sidebar.close')" @click="emit('close')">
            <PanelLeftClose :size="18" />
          </ZIconButton>

          <ZIconButton
            v-else
            :size="isCollapsed ? 'default' : 'small'"
            data-sidebar-toggle
            :aria-label="isCollapsed ? t('navigation.sidebar.expand') : t('navigation.sidebar.collapse')"
            @click="emit('toggle')"
          >
            <PanelLeftOpen v-if="isCollapsed" :size="19" />
            <PanelLeftClose v-else :size="18" />
          </ZIconButton>
        </div>
      </div>
    </template>
  </ZSidebar>
</template>
