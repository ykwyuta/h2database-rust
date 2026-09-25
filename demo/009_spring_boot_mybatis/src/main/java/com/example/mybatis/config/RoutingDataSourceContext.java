package com.example.mybatis.config;

public class RoutingDataSourceContext {
    private static final ThreadLocal<DataSourceType> CONTEXT = new ThreadLocal<>();

    public enum DataSourceType {
        PRIMARY,
        STANDBY
    }

    public static void set(DataSourceType type) {
        CONTEXT.set(type);
    }

    public static DataSourceType get() {
        return CONTEXT.get() != null ? CONTEXT.get() : DataSourceType.PRIMARY;
    }

    public static void clear() {
        CONTEXT.remove();
    }
}
