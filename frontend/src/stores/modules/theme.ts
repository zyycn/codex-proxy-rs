// @env browser

import type {
  Theme,
  ThemeColorId,
  ThemeCustomization,
  ThemeMode,
  ThemePresetId,
} from '@codex-proxy/ui/theme'

import { normalizeHexColor } from '@codex-proxy/ui'
import {
  applyTheme,
  createTheme,
  defaultThemeColorId,
  defaultThemeMode,
  defaultThemePresetId,
  defaultThemePrimaryColor,
  isThemeColorId,
  isThemeMode,
  isThemePresetId,
  normalizeThemeCustomization,
  resolveThemeScheme,
} from '@codex-proxy/ui/theme'
import { usePreferredDark, usePreferredReducedMotion, useTimeoutFn } from '@vueuse/core'

import { defineStore } from 'pinia'
import { computed, ref, shallowRef, watch } from 'vue'

interface ThemeTransitionOrigin {
  x: number
  y: number
}

interface ThemeConfigurationInput {
  preset: ThemePresetId
  mode: ThemeMode
  color: ThemeColorId
  customColor: string
  customization: ThemeCustomization
}

interface ViewTransition {
  ready: Promise<void>
  finished: Promise<void>
}

type ViewTransitionDocument = Document & {
  startViewTransition?: (callback: () => void) => ViewTransition
}

export const useThemeStore = defineStore(
  'theme',
  () => {
    const themeMode = shallowRef<ThemeMode>(defaultThemeMode)
    const themePreset = shallowRef<ThemePresetId>(defaultThemePresetId)
    const themeColor = shallowRef<ThemeColorId>(defaultThemeColorId)
    const customThemeColor = shallowRef(defaultThemePrimaryColor)
    const themeCustomization = ref<ThemeCustomization>({})
    const themeRevision = shallowRef(0)
    const themeTransitioning = shallowRef(false)
    const preferredDark = usePreferredDark()
    const preferredMotion = usePreferredReducedMotion()
    let appliedThemeSignature: string | undefined
    let themeTransitionOrigin: ThemeTransitionOrigin | undefined
    let themeTransitionRequested = false

    const effectiveTheme = computed(() => resolveThemeScheme(themeMode.value, preferredDark.value))
    const resolvedTheme = computed(() =>
      createTheme({
        scheme: effectiveTheme.value,
        preset: themePreset.value,
        colorId: themeColor.value,
        primaryColor: customThemeColor.value,
        customization: themeCustomization.value,
      }),
    )

    const { start: startFallbackTransitionTimer, stop: stopFallbackTransitionTimer } = useTimeoutFn(
      () => {
        document.documentElement.classList.remove('theme-fallback-transition')
        themeTransitioning.value = false
      },
      180,
      { immediate: false },
    )

    watch(resolvedTheme, theme => syncTheme(theme))

    function initializeTheme(): void {
      normalizePersistedTheme()
      syncTheme(resolvedTheme.value)
    }

    function normalizePersistedTheme(): void {
      if (!isThemePresetId(themePreset.value))
        themePreset.value = defaultThemePresetId
      if (!isThemeMode(themeMode.value))
        themeMode.value = defaultThemeMode
      if (!isThemeColorId(themeColor.value))
        themeColor.value = defaultThemeColorId
      customThemeColor.value = normalizeHexColor(customThemeColor.value) ?? defaultThemePrimaryColor
      themeCustomization.value = normalizeThemeCustomization(themeCustomization.value)
    }

    function syncTheme(theme: Theme): void {
      if (typeof document === 'undefined')
        return

      const signature = themeSignature(theme)
      if (appliedThemeSignature === signature) {
        clearThemeTransitionRequest()
        return
      }

      if (
        appliedThemeSignature === undefined
        || !themeTransitionRequested
        || preferredMotion.value === 'reduce'
        || themeTransitioning.value
      ) {
        commitTheme(theme)
        clearThemeTransitionRequest()
        return
      }

      runThemeTransition(theme)
      clearThemeTransitionRequest()
    }

    function commitTheme(theme: Theme): void {
      applyTheme(document.documentElement, theme)
      document.querySelector('meta[name="theme-color"]')?.setAttribute('content', theme.tokens['color-bg-container'])
      appliedThemeSignature = themeSignature(theme)
      themeRevision.value += 1
    }

    function runThemeTransition(theme: Theme): void {
      const transitionDocument = document as ViewTransitionDocument
      const shrinkingDarkLayer
        = document.documentElement.dataset.theme === 'dark' && theme.scheme === 'light'

      if (!transitionDocument.startViewTransition) {
        runFallbackThemeTransition(theme)
        return
      }

      const origin = themeTransitionOrigin ?? {
        x: window.innerWidth - 44,
        y: 44,
      }
      const maxX = Math.max(origin.x, window.innerWidth - origin.x)
      const maxY = Math.max(origin.y, window.innerHeight - origin.y)
      const radius = Math.hypot(maxX, maxY)
      document.documentElement.classList.toggle('theme-view-transition-shrink', shrinkingDarkLayer)
      themeTransitioning.value = true
      let transition: ViewTransition
      try {
        transition = transitionDocument.startViewTransition(() => commitTheme(theme))
      }
      catch {
        document.documentElement.classList.remove('theme-view-transition-shrink')
        themeTransitioning.value = false
        runFallbackThemeTransition(theme)
        return
      }

      let activeAnimation: Animation | undefined
      void transition.finished
        .catch(() => undefined)
        .finally(() => {
          // 旧快照移除后再取消裁剪，避免深色图层在最后一帧重新铺满页面。
          activeAnimation?.cancel()
          document.documentElement.classList.remove('theme-view-transition-shrink')
          themeTransitioning.value = false
        })
      void transition.ready
        .then(() => {
          activeAnimation = document.documentElement.animate(
            {
              clipPath: shrinkingDarkLayer
                ? [
                    `circle(${radius}px at ${origin.x}px ${origin.y}px)`,
                    `circle(0px at ${origin.x}px ${origin.y}px)`,
                  ]
                : [
                    `circle(0px at ${origin.x}px ${origin.y}px)`,
                    `circle(${radius}px at ${origin.x}px ${origin.y}px)`,
                  ],
            },
            {
              duration: 420,
              easing: 'cubic-bezier(0.22, 1, 0.36, 1)',
              fill: 'both',
              pseudoElement: shrinkingDarkLayer
                ? '::view-transition-old(root)'
                : '::view-transition-new(root)',
            },
          )
        })
        .catch(() => undefined)
    }

    function runFallbackThemeTransition(theme: Theme): void {
      themeTransitioning.value = true
      stopFallbackTransitionTimer()
      document.documentElement.classList.add('theme-fallback-transition')
      commitTheme(theme)
      startFallbackTransitionTimer()
    }

    function setThemeMode(mode: ThemeMode, event?: MouseEvent): void {
      if (mode === themeMode.value) {
        clearThemeTransitionRequest()
        return
      }
      requestThemeTransition(event)
      themeMode.value = mode
    }

    function setThemeColor(color: ThemeColorId, event?: MouseEvent): void {
      if (color === themeColor.value) {
        clearThemeTransitionRequest()
        return
      }
      requestThemeTransition(event)
      themeColor.value = color
    }

    function setCustomThemeColor(color: string, event?: MouseEvent): boolean {
      const normalized = normalizeHexColor(color)
      if (!normalized)
        return false
      if (themeColor.value === 'custom' && customThemeColor.value === normalized) {
        clearThemeTransitionRequest()
        return true
      }
      requestThemeTransition(event)
      customThemeColor.value = normalized
      themeColor.value = 'custom'
      return true
    }

    function setThemeConfiguration(
      configuration: ThemeConfigurationInput,
      event?: MouseEvent,
    ): boolean {
      const customColor = normalizeHexColor(configuration.customColor)
      if (!isThemePresetId(configuration.preset) || !isThemeMode(configuration.mode) || !isThemeColorId(configuration.color) || !customColor)
        return false

      requestThemeTransition(event)
      themePreset.value = configuration.preset
      themeMode.value = configuration.mode
      themeColor.value = configuration.color
      customThemeColor.value = customColor
      themeCustomization.value = normalizeThemeCustomization(configuration.customization)
      return true
    }

    function resetTheme(event?: MouseEvent): void {
      const unchanged
        = themeMode.value === defaultThemeMode
          && themePreset.value === defaultThemePresetId
          && themeColor.value === defaultThemeColorId
          && customThemeColor.value === defaultThemePrimaryColor
          && Object.keys(themeCustomization.value).length === 0
      if (unchanged) {
        clearThemeTransitionRequest()
        return
      }
      requestThemeTransition(event)
      themeMode.value = defaultThemeMode
      themePreset.value = defaultThemePresetId
      themeColor.value = defaultThemeColorId
      customThemeColor.value = defaultThemePrimaryColor
      themeCustomization.value = {}
    }

    function toggleTheme(event?: MouseEvent): void {
      if (themeTransitioning.value)
        return
      requestThemeTransition(event)
      themeMode.value = effectiveTheme.value === 'dark'
        ? 'light'
        : 'dark'
    }

    function requestThemeTransition(event?: MouseEvent): void {
      themeTransitionOrigin = event
        ? { x: event.clientX, y: event.clientY }
        : undefined
      themeTransitionRequested = Boolean(event)
    }

    function clearThemeTransitionRequest(): void {
      themeTransitionOrigin = undefined
      themeTransitionRequested = false
    }

    return {
      themeMode,
      themePreset,
      themeColor,
      customThemeColor,
      themeCustomization,
      themeRevision,
      themeTransitioning,
      effectiveTheme,
      initializeTheme,
      setThemeMode,
      setThemeColor,
      setCustomThemeColor,
      setThemeConfiguration,
      resetTheme,
      toggleTheme,
    }
  },
  {
    persist: {
      key: 'codex-proxy-rs-theme',
      pick: [
        'themeMode',
        'themePreset',
        'themeColor',
        'customThemeColor',
        'themeCustomization',
      ],
    },
  },
)

function themeSignature(theme: Theme): string {
  return `${theme.preset}:${theme.scheme}:${theme.colorId}:${theme.seed}:${JSON.stringify(theme.tokens)}`
}
