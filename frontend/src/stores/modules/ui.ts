import { useDark, usePreferredReducedMotion, useTimeoutFn } from '@vueuse/core'
import { defineStore } from 'pinia'
import { computed, shallowRef } from 'vue'

type ThemeMode = 'system' | 'light' | 'dark'
type ThemeName = 'light' | 'dark'
type ColorModeStorageValue = 'auto' | ThemeName
interface ThemeTransitionOrigin { x: number, y: number }
interface ViewTransition {
  ready: Promise<void>
  finished: Promise<void>
}

type ViewTransitionDocument = Document & {
  startViewTransition?: (callback: () => void) => ViewTransition
}

export const useUiStore = defineStore(
  'ui',
  () => {
    const sidebarCollapsed = shallowRef(false)
    const themeMode = shallowRef<ThemeMode>('system')
    const themeRevision = shallowRef(0)
    const themeTransitioning = shallowRef(false)
    const preferredMotion = usePreferredReducedMotion()
    let themeApplied = false
    let themeTransitionOrigin: ThemeTransitionOrigin | undefined
    let themeTransitionRequested = false

    const { start: startFallbackTransitionTimer, stop: stopFallbackTransitionTimer } = useTimeoutFn(
      () => {
        document.documentElement.classList.remove('theme-fallback-transition')
        themeTransitioning.value = false
      },
      180,
      { immediate: false },
    )

    const colorModeStorage = computed<ColorModeStorageValue>({
      get: () => (themeMode.value === 'system' ? 'auto' : themeMode.value),
      set: (mode) => {
        themeMode.value = mode === 'auto' ? 'system' : mode
      },
    })

    const isDark = useDark({
      storageRef: colorModeStorage,
      onChanged: (dark) => {
        applyTheme(dark ? 'dark' : 'light')
      },
    })

    const effectiveTheme = computed<ThemeName>(() => (isDark.value ? 'dark' : 'light'))

    function toggleSidebar() {
      sidebarCollapsed.value = !sidebarCollapsed.value
    }

    function applyTheme(theme: ThemeName) {
      if (themeApplied && document.documentElement.dataset.theme === theme) {
        themeTransitionRequested = false
        return
      }

      if (
        !themeApplied
        || !themeTransitionRequested
        || preferredMotion.value === 'reduce'
        || themeTransitioning.value
      ) {
        commitTheme(theme)
        themeApplied = true
        themeTransitionRequested = false
        return
      }

      runThemeTransition(theme)
      themeApplied = true
      themeTransitionRequested = false
    }

    function commitTheme(theme: ThemeName) {
      document.documentElement.dataset.theme = theme
      document.documentElement.style.colorScheme = theme
      themeRevision.value += 1
    }

    function runThemeTransition(theme: ThemeName) {
      const transitionDocument = document as ViewTransitionDocument
      const shrinkingDarkLayer
        = document.documentElement.dataset.theme === 'dark' && theme === 'light'

      if (!transitionDocument.startViewTransition) {
        runFallbackThemeTransition(theme)
        return
      }

      const origin = themeTransitionOrigin ?? {
        x: window.innerWidth - 44,
        y: 44,
      }
      themeTransitionOrigin = undefined
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
      const cleanupTransition = () => {
        if (cleaned) {
          return
        }
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

    function runFallbackThemeTransition(theme: ThemeName) {
      themeTransitioning.value = true
      stopFallbackTransitionTimer()
      document.documentElement.classList.add('theme-fallback-transition')
      commitTheme(theme)
      startFallbackTransitionTimer()
    }

    function initializeTheme() {
      if (!themeApplied) {
        applyTheme(effectiveTheme.value)
      }
    }

    function setThemeMode(mode: ThemeMode) {
      themeMode.value = mode
    }

    function toggleTheme(event?: MouseEvent) {
      if (themeTransitioning.value) {
        return
      }

      if (event) {
        themeTransitionOrigin = {
          x: event.clientX,
          y: event.clientY,
        }
        themeTransitionRequested = true
      }
      themeMode.value = effectiveTheme.value === 'dark' ? 'light' : 'dark'
    }

    return {
      sidebarCollapsed,
      themeMode,
      themeRevision,
      themeTransitioning,
      effectiveTheme,
      toggleSidebar,
      initializeTheme,
      setThemeMode,
      toggleTheme,
    }
  },
  {
    persist: {
      key: 'codex-proxy-rs-ui',
      pick: ['sidebarCollapsed', 'themeMode'],
    },
  },
)
