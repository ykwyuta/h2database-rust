package com.example.mybatis.mapper;

import com.example.mybatis.model.Subscription;
import org.apache.ibatis.annotations.Param;

import java.util.List;

public interface SubscriptionMapper {

    void createSubscriptionTable();

    int insertSubscription(@Param("userId") Long userId, @Param("planName") String planName);

    List<Subscription> findAllSubscriptions();
}
