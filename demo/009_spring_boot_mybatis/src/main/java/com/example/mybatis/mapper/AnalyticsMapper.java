package com.example.mybatis.mapper;

import com.example.mybatis.model.AnalyticsMetric;
import org.apache.ibatis.annotations.Param;

import java.util.List;

public interface AnalyticsMapper {

    void createAnalyticsTable();

    int insertMetric(@Param("name") String name, @Param("val") Double val);

    List<AnalyticsMetric> calculateAdvancedMetrics();

    List<AnalyticsMetric> searchByRegex(@Param("pattern") String pattern);
}
