<script setup lang="ts">
import { RouterView } from 'vue-router'

import { BaseToast } from '@/components/base/BaseToast'
</script>

<template>
  <RouterView />
  <BaseToast />
</template>
