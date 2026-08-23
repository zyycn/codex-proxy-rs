// @env browser

import type {
  ResolvedTheme,
  ThemeColorId,
  ThemeCustomization,
  ThemeMode,
} from '@/theme'

import { usePreferredDark, usePreferredReducedMotion, useTimeoutFn } from '@vueuse/core'
import { defineStore } from 'pinia'
import { computed, ref, shallowRef, watch } from 'vue'

import {
  applyResolvedTheme,
  DEFAULT_CUSTOM_THEME_COLOR,
  DEFAULT_THEME_COLOR,
  DEFAULT_THEME_MODE,
  isThemeColorId,
  isThemeMode,
  normalizeThemeCustomization,
  resolveTheme,
  resolveThemeName,
} from '@/theme'
import { normalizeHexColor } from '@/utils/color'

interface ThemeTransitionOrigin {
  x: number
  y: number
}

interface ThemeConfigurationInput {
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
    const themeMode = shallowRef<ThemeMode>(DEFAULT_THEME_MODE)
    const themeColor = shallowRef<ThemeColorId>(DEFAULT_THEME_COLOR)
    const customThemeColor = shallowRef(DEFAULT_CUSTOM_THEME_COLOR)
    const themeCustomization = ref<ThemeCustomization>({})
    const themeRevision = shallowRef(0)
    const themeTransitioning = shallowRef(false)
    const preferredDark = usePreferredDark()
    const preferredMotion = usePreferredReducedMotion()
    let appliedThemeSignature: string | undefined
    let themeTransitionOrigin: ThemeTransitionOrigin | undefined
    let themeTransitionRequested = false

    const effectiveTheme = computed(() => resolveThemeName(themeMode.value, preferredDark.value))
    const resolvedTheme = computed(() =>
      resolveTheme(
        effectiveTheme.value,
        themeColor.value,
        customThemeColor.value,
        themeCustomization.value,
      ),
    )

    const { start: startFallbackTransitionTimer, stop: stopFallbackTransitionTimer } = useTimeoutFn(
      () => {
        document.documentElement.classList.remove('theme-fallback-transition')
        themeTransitioning.value = false
      },
      180,
      { immediate: false },
    )

    watch(resolvedTheme, theme => applyTheme(theme))

    function initializeTheme(): void {
      normalizePersistedTheme()
      applyTheme(resolvedTheme.value)
    }

    function normalizePersistedTheme(): void {
      if (!isThemeMode(themeMode.value))
        themeMode.value = DEFAULT_THEME_MODE
      if (!isThemeColorId(themeColor.value))
        themeColor.value = DEFAULT_THEME_COLOR
      customThemeColor.value = normalizeHexColor(customThemeColor.value) ?? DEFAULT_CUSTOM_THEME_COLOR
      themeCustomization.value = normalizeThemeCustomization(themeCustomization.value)
    }

    function applyTheme(theme: ResolvedTheme): void {
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

    function commitTheme(theme: ResolvedTheme): void {
      applyResolvedTheme(document.documentElement, theme)
      appliedThemeSignature = themeSignature(theme)
      themeRevision.value += 1
    }

    function runThemeTransition(theme: ResolvedTheme): void {
      const transitionDocument = document as ViewTransitionDocument
      const shrinkingDarkLayer
        = document.documentElement.dataset.theme === 'dark' && theme.name === 'light'

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

      let cleaned = false
      const cleanupTransition = (): void => {
        if (cleaned)
          return
        cleaned = true
        requestAnimationFrame(() => {
          document.documentElement.classList.remove('theme-view-transition-shrink')
          themeTransitioning.value = false
        })
      }
      const cleanupTimer = window.setTimeout(cleanupTransition, 1200)
      void transition.finished.catch(() => undefined)
      let activeAnimation: Animation | undefined
      void transition.ready
        .then(() => {
          const animation = document.documentElement.animate(
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
          activeAnimation = animation
          return animation.finished.catch(() => undefined)
        })
        .catch(() => undefined)
        .finally(() => {
          activeAnimation?.cancel()
          window.clearTimeout(cleanupTimer)
          cleanupTransition()
        })
    }

    function runFallbackThemeTransition(theme: ResolvedTheme): void {
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
      if (!isThemeMode(configuration.mode) || !isThemeColorId(configuration.color) || !customColor)
        return false

      requestThemeTransition(event)
      themeMode.value = configuration.mode
      themeColor.value = configuration.color
      customThemeColor.value = customColor
      themeCustomization.value = normalizeThemeCustomization(configuration.customization)
      return true
    }

    function resetTheme(event?: MouseEvent): void {
      const unchanged
        = themeMode.value === DEFAULT_THEME_MODE
          && themeColor.value === DEFAULT_THEME_COLOR
          && customThemeColor.value === DEFAULT_CUSTOM_THEME_COLOR
          && Object.keys(themeCustomization.value).length === 0
      if (unchanged) {
        clearThemeTransitionRequest()
        return
      }
      requestThemeTransition(event)
      themeMode.value = DEFAULT_THEME_MODE
      themeColor.value = DEFAULT_THEME_COLOR
      customThemeColor.value = DEFAULT_CUSTOM_THEME_COLOR
      themeCustomization.value = {}
    }

    function toggleTheme(event?: MouseEvent): void {
      if (themeTransitioning.value)
        return
      requestThemeTransition(event)
      themeMode.value = effectiveTheme.value === 'dark' ? 'light' : 'dark'
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
        'themeColor',
        'customThemeColor',
        'themeCustomization',
      ],
    },
  },
)

function themeSignature(theme: ResolvedTheme): string {
  return `${theme.name}:${theme.colorId}:${theme.seed}:${JSON.stringify(theme.tokens)}`
}
