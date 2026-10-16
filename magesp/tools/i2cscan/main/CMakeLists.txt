idf_component_register(SRCS "i2cscan.c"
    REQUIRES driver esp_driver_i2c esp_wifi esp_netif esp_event nvs_flash)
