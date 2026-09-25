package com.example.mybatis.config;

import org.aspectj.lang.ProceedingJoinPoint;
import org.aspectj.lang.annotation.Around;
import org.aspectj.lang.annotation.Aspect;
import org.springframework.core.annotation.Order;
import org.springframework.stereotype.Component;
import org.springframework.transaction.annotation.Transactional;

@Aspect
@Component
@Order(0) // トランザクション開始前にデータソースを決定
public class ReadOnlyRouteAspect {

    @Around("@annotation(transactional)")
    public Object proceedWithRoute(ProceedingJoinPoint pjp, Transactional transactional) throws Throwable {
        RoutingDataSourceContext.DataSourceType previous = RoutingDataSourceContext.get();
        try {
            if (transactional.readOnly()) {
                RoutingDataSourceContext.set(RoutingDataSourceContext.DataSourceType.STANDBY);
            } else {
                RoutingDataSourceContext.set(RoutingDataSourceContext.DataSourceType.PRIMARY);
            }
            return pjp.proceed();
        } finally {
            RoutingDataSourceContext.set(previous);
        }
    }
}
