import { defineStore } from 'pinia'
import { computed, shallowRef, watch } from 'vue'

type ThemeMode = 'system' | 'light' | 'dark'
type ThemeName = 'light' | 'dark'
type ThemeTransitionOrigin = { x: number; y: number }
type ViewTransition = {
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
    const systemTheme = shallowRef<ThemeName>(preferredTheme())

    const effectiveTheme = computed<ThemeName>(() =>
      themeMode.value === 'system' ? systemTheme.value : themeMode.value,
    )

    let mediaQuery: MediaQueryList | undefined
    let listeningTheme = false
    let themeApplied = false
    let themeTransitionTimer: number | undefined
    let themeTransitionOrigin: ThemeTransitionOrigin | undefined
    let themeTransitionRequested = false

    function toggleSidebar() {
      sidebarCollapsed.value = !sidebarCollapsed.value
    }

    function preferredTheme(): ThemeName {
      return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
    }

    function applyTheme(theme: ThemeName) {
      if (
        !themeApplied ||
        !themeTransitionRequested ||
        window.matchMedia('(prefers-reduced-motion: reduce)').matches
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
    }

    function runThemeTransition(theme: ThemeName) {
      const transitionDocument = document as ViewTransitionDocument
      const previousTheme = document.documentElement.dataset.theme
      const expanding = theme === 'dark'

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
      document.documentElement.classList.toggle(
        'theme-view-transition-reverse',
        previousTheme === 'dark' && theme === 'light',
      )
      const transition = transitionDocument.startViewTransition(() => commitTheme(theme))

      transition.ready.then(() => {
        document.documentElement.animate(
          {
            clipPath: expanding
              ? [
                  `circle(0px at ${origin.x}px ${origin.y}px)`,
                  `circle(${radius}px at ${origin.x}px ${origin.y}px)`,
                ]
              : [
                  `circle(${radius}px at ${origin.x}px ${origin.y}px)`,
                  `circle(0px at ${origin.x}px ${origin.y}px)`,
                ],
          },
          {
            duration: 420,
            easing: 'cubic-bezier(0.22, 1, 0.36, 1)',
            fill: 'both',
            pseudoElement: expanding
              ? '::view-transition-new(root)'
              : '::view-transition-old(root)',
          },
        )
      })
      transition.finished.finally(() => {
        requestAnimationFrame(() => {
          document.documentElement.classList.remove('theme-view-transition-reverse')
        })
      })
    }

    function runFallbackThemeTransition(theme: ThemeName) {
      window.clearTimeout(themeTransitionTimer)
      document.documentElement.classList.add('theme-fallback-transition')
      commitTheme(theme)
      themeTransitionTimer = window.setTimeout(() => {
        document.documentElement.classList.remove('theme-fallback-transition')
      }, 180)
    }

    function initializeTheme() {
      mediaQuery = window.matchMedia('(prefers-color-scheme: dark)')
      systemTheme.value = mediaQuery.matches ? 'dark' : 'light'

      if (listeningTheme) return

      mediaQuery.addEventListener('change', (event) => {
        systemTheme.value = event.matches ? 'dark' : 'light'
      })
      listeningTheme = true
    }

    function setThemeMode(mode: ThemeMode) {
      themeMode.value = mode
    }

    function toggleTheme(event?: MouseEvent) {
      if (event) {
        themeTransitionOrigin = {
          x: event.clientX,
          y: event.clientY,
        }
        themeTransitionRequested = true
      }
      themeMode.value = effectiveTheme.value === 'dark' ? 'light' : 'dark'
    }

    watch(effectiveTheme, applyTheme, { immediate: true })

    return {
      sidebarCollapsed,
      themeMode,
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
