<script setup lang="ts">
import { RouterView } from 'vue-router'

import { BaseToast } from '@/components/base/BaseToast'
import { useUiStore } from '@/stores/modules/ui'

useUiStore().initializeTheme()
</script>

<template>
  <RouterView />
  <BaseToast />
</template>
